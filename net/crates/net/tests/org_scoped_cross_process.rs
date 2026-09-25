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
//! 1. the child's EMISSION actually carries the granted envelope and its
//!    handler dispatched EXACTLY ONCE (`RESULT ok calls=<measured, pinned to 1>
//!    emitted=<n>` — the `SendEmission.scoped` cache site; the count is read
//!    from the `AttributingHandler`'s counter, never a literal (TESTS-3));
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

/// Bounds for the three formerly-unbounded waits in this harness (TESTS-2):
/// the parent's READY/RESULT pulls and the child's `accept`. Generous against
/// CI saturation (the child's own call deadline is 90 s), but finite — a
/// wedged side becomes a named red, never a hung job.
const CHILD_PROTOCOL_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(60);

/// TESTS-2: the spawned provider child must not outlive ANY parent verdict.
/// The success path hands the exit status over via [`Self::wait_clean`]; every
/// other path — including an `assert!` panic unwinding out of the test — drops
/// this guard, which kills AND `wait()`s the child. `kill` without `wait`
/// leaves the process object unreaped; neither leaves a live orphan serving
/// against a red parent.
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn spawn(cmd: &mut Command) -> Self {
        Self(Some(cmd.spawn().expect("spawn provider child process")))
    }

    /// The child's captured stdout (taken exactly once, at spawn).
    fn stdout(&mut self) -> std::process::ChildStdout {
        self.0
            .as_mut()
            .expect("child not yet reaped")
            .stdout
            .take()
            .expect("child stdout")
    }

    /// The green path: reap the child WITHOUT killing it first.
    fn wait_clean(mut self) -> std::process::ExitStatus {
        self.0
            .take()
            .expect("child not yet reaped")
            .wait()
            .expect("child exit")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// TESTS-2: the child's stdout on a pump thread, so every parent pull can be
/// bounded. The pump forwards lines until EOF (child exit) or until the parent
/// stops listening; a `read_line` here can block forever only while the child
/// lives — and the `ChildGuard` reaps the child on every parent exit.
fn pump_child_stdout(stdout: std::process::ChildStdout) -> std::sync::mpsc::Receiver<String> {
    use std::io::BufRead;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if tx.send(line.clone()).is_err() {
                break;
            }
        }
    });
    rx
}

/// TESTS-2: one bounded pull of a child-protocol line. `Disconnected` means
/// the child closed its stdout (it exited); `Timeout` means it wedged. Both
/// panic — the `ChildGuard` reaps the child as the panic unwinds.
fn next_child_line(
    rx: &std::sync::mpsc::Receiver<String>,
    deadline: Instant,
    what: &str,
) -> String {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(remaining) {
        Ok(line) => line,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("child exited before {what} (its stdout closed)")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("child produced no {what} within {CHILD_PROTOCOL_TIMEOUT:?} (bounded read)")
        }
    }
}

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
    let mut cfg =
        MeshNodeConfig::new(addr, PSK).with_heartbeat_interval(Duration::from_millis(200));
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
    //
    // TESTS-2: the accept is BOUNDED. A parent that dies between spawn and
    // connect (or never connects at all) must not leave this child parked in
    // `accept` forever — the harness idiom re-executes this exact binary, so
    // an orphan blocks the `--test-threads=1` run it was spawned into.
    let accept_node = provider.clone();
    let accept = tokio::spawn(async move { accept_node.accept(caller_node).await });

    println!(
        "READY {} {} {}",
        provider.local_addr(),
        encode_hex32(provider.public_key()),
        provider.node_id()
    );

    match tokio::time::timeout(ACCEPT_TIMEOUT, accept).await {
        Ok(joined) => {
            if let Err(e) = joined.expect("accept task") {
                fail(format!("accept: {e}"));
            }
        }
        Err(_) => fail(format!(
            "accept: no inbound session within {ACCEPT_TIMEOUT:?} (bounded accept)"
        )),
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
    let _serve: ServeHandle = match provider.serve_rpc_granted(SERVICE, handler, Arc::new(|_| true))
    {
        Ok(h) => h,
        Err(e) => fail(format!("granted serve: {e:?}")),
    };

    // Announce loop + emission instrumentation: the `SendEmission.scoped`
    // cache site, observed from the provider side and reported on the RESULT
    // line either way, so a red names its own mechanism.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut emitted = 0usize;
    while Instant::now() < deadline {
        provider
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
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
    // TESTS-3: the MEASURED dispatch count, never a literal. The parent pins
    // `calls=1` on this marker; a duplicate dispatch (calls=2) must surface
    // here instead of hiding behind a hard-coded `1`.
    let observed = calls.load(Ordering::SeqCst);
    println!("RESULT ok calls={observed} emitted={emitted}");
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
    let caller_authority = NodeAuthority::adopt(
        &dir.join("caller_authority"),
        caller_cert,
        &caller_entity,
        0,
        None,
    )
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
    //
    // TESTS-2: the child is wrapped in `ChildGuard` the moment it exists, so
    // EVERY parent verdict below — including a plain `assert!` panic — kills
    // AND `wait()`s it on the way out (a bare `kill` without `wait` leaves the
    // process object unreaped; a panic without either orphans a child that
    // keeps running against a parent that is already red).
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = ChildGuard::spawn(
        Command::new(exe)
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
            .stderr(Stdio::inherit()),
    );
    let line_rx = pump_child_stdout(child.stdout());

    // READY handshake with the child. The spawned binary is a libtest harness,
    // so its own output precedes the child's protocol lines — and its
    // `test <name> ... ` progress line carries NO trailing newline, so the
    // child's first println CONCATENATES onto it. Scan for the READY marker
    // anywhere in a line and slice from there; fail closed on a terminal
    // RESULT before it. Every pull is bounded (TESTS-2): the pump thread owns
    // the pipe, and `recv_timeout` is what turns a wedged child into a named
    // red instead of a hung job.
    let ready_line = {
        let ready_deadline = Instant::now() + CHILD_PROTOCOL_TIMEOUT;
        loop {
            let line = next_child_line(&line_rx, ready_deadline, "READY");
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
        // No manual `kill` here (TESTS-2): the `ChildGuard` kills AND reaps
        // the child as this panic unwinds, on this and every other red path.
        panic!("F-S4Vectors-1: scoped discovery did not cross the OS-process boundary");
    }
    let providers = caller.granted_capability_providers(&grant.grant_id);
    assert_eq!(
        providers.len(),
        1,
        "exactly the one granted provider considered"
    );
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
        capability,
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
    // noise tolerance as the READY scan, same bounded pull (TESTS-2).
    let result_line = {
        let result_deadline = Instant::now() + CHILD_PROTOCOL_TIMEOUT;
        loop {
            let line = next_child_line(&line_rx, result_deadline, "RESULT");
            let trimmed = line.trim();
            if let Some(idx) = trimmed.find("RESULT ") {
                break trimmed[idx..].to_string();
            }
        }
    };
    // `calls=1` is pinned against the child's MEASURED dispatch count
    // (TESTS-3): a duplicated dispatch prints `calls=2` and reddens here.
    assert!(
        result_line.trim().starts_with("RESULT ok calls=1 emitted="),
        "the provider child verified its own handler attribution + emission \
         with exactly one handler dispatch (duplicate dispatch must not pass); got {result_line:?}"
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

    let status = child.wait_clean();
    assert!(status.success(), "provider child exited cleanly");
    let _ = std::fs::remove_dir_all(&dir);
}
