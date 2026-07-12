//! Helper node for the Linux netns NAT-simulator suite
//! (`tests/natsim/`, `NAT_TRAVERSAL_V2_PLAN.md` Stage 4).
//!
//! One process = one mesh node inside one network namespace. The
//! scenario scripts launch several of these (`ip netns exec …`) and
//! coordinate them through a shared state directory (namespaces
//! share the filesystem): each node writes `<name>.json` with its
//! identity, publics hand out accept-turn markers so `accept()`'s
//! expected-node-id contract holds, and the initiator writes an
//! `outcome.json` verdict that the `tests/natsim.rs` wrappers
//! assert on.
//!
//! Roles:
//! - `keygen`  — print a fresh identity (seed + node_id) so scripts
//!   can order two joiners by node id (the upgrade scenario needs
//!   the NAT'd joiner to be the C1 lower-id initiator).
//! - `public`  — publicly-addressed node (relay / classification
//!   target). Accepts the named joiners in file-coordinated order,
//!   then serves until killed.
//! - `joiner`  — a node that dials the publics, classifies,
//!   announces, and (optionally) drives a punch / upgrade toward a
//!   target joiner, writing the outcome.
//!
//! Build: `cargo build --example natsim_node --features net,nat-traversal`
//! Not intended to run outside the natsim harness.

#![cfg_attr(not(all(feature = "net", feature = "nat-traversal")), allow(unused))]

/// Feature-gated stub: an example must always have a `main`.
#[cfg(not(all(feature = "net", feature = "nat-traversal")))]
fn main() {
    eprintln!("natsim_node requires --features net,nat-traversal");
    std::process::exit(2);
}

#[cfg(all(feature = "net", feature = "nat-traversal"))]
mod natsim {

    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use net::adapter::net::behavior::capability::CapabilitySet;
    use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

    const PSK: [u8; 32] = [0x42u8; 32];
    /// How long coordination waits (files, reflex visibility) may take.
    const COORD_TIMEOUT: Duration = Duration::from_secs(60);
    // How long a joiner keeps re-running the classification sweep
    // waiting for a concrete NAT class before giving up and proceeding.
    const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);

    fn usage() -> ! {
        eprintln!(
            "usage:\n  natsim_node keygen\n  natsim_node public --name N --bind IP:PORT \
         --state DIR --joiners a,b [--connect-to x]\n  natsim_node joiner --name N \
         --bind IP:PORT --state DIR --publics r,x [--seed-hex H] [--auto-upgrade] \
         [--target N --mode punch|upgrade] "
        );
        std::process::exit(2);
    }

    fn parse_flags(args: &[String]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let mut i = 0;
        while i < args.len() {
            let key = args[i].trim_start_matches("--").to_string();
            if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                out.insert(key, args[i + 1].clone());
                i += 2;
            } else {
                out.insert(key, String::from("true"));
                i += 1;
            }
        }
        out
    }

    fn random_seed() -> [u8; 32] {
        // The harness runs on Linux (and builds on macOS): /dev/urandom
        // is present on both. open + read_exact — a whole-file `read`
        // would never see EOF.
        use std::io::Read;
        let mut seed = [0u8; 32];
        let mut f = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
        f.read_exact(&mut seed).expect("read urandom");
        seed
    }

    fn keypair_from(flags: &HashMap<String, String>) -> EntityKeypair {
        match flags.get("seed-hex") {
            Some(h) => {
                let bytes = hex::decode(h).expect("seed-hex must be 64 hex chars");
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&bytes);
                EntityKeypair::from_bytes(seed)
            }
            None => EntityKeypair::from_bytes(random_seed()),
        }
    }

    fn node_config(bind: SocketAddr, auto_upgrade: bool) -> MeshNodeConfig {
        let mut cfg = MeshNodeConfig::new(bind, PSK)
            .with_heartbeat_interval(Duration::from_millis(500))
            .with_session_timeout(Duration::from_secs(10))
            .with_handshake(4, Duration::from_secs(3));
        // Announcements must be re-broadcastable promptly — the harness
        // announces once per node, but the reflex-diff trigger and late
        // joiners lean on the re-announce loop.
        cfg.min_announce_interval = Duration::from_millis(100);
        if auto_upgrade {
            cfg = cfg.with_auto_direct_upgrade(true);
        }
        cfg
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct NodeInfo {
        name: String,
        node_id: u64,
        pubkey_hex: String,
        addr: String,
    }

    async fn wait_for_file(path: &Path) -> Vec<u8> {
        let start = tokio::time::Instant::now();
        loop {
            if let Ok(bytes) = std::fs::read(path) {
                if !bytes.is_empty() {
                    return bytes;
                }
            }
            if start.elapsed() > COORD_TIMEOUT {
                eprintln!("natsim_node: timed out waiting for {}", path.display());
                std::process::exit(3);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_info(state: &Path, name: &str) -> NodeInfo {
        let bytes = wait_for_file(&state.join(format!("{name}.json"))).await;
        serde_json::from_slice(&bytes).expect("malformed node info json")
    }

    async fn wait_for_marker(state: &Path, marker: &str) {
        wait_for_file(&state.join(marker)).await;
    }

    fn write_atomic(path: &Path, bytes: &[u8]) {
        // Write-then-rename so readers polling the path never observe a
        // partial file.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).expect("write state file");
        std::fs::rename(&tmp, path).expect("rename state file");
    }

    fn write_info(state: &Path, info: &NodeInfo) {
        write_atomic(
            &state.join(format!("{}.json", info.name)),
            &serde_json::to_vec_pretty(info).unwrap(),
        );
    }

    fn write_marker(state: &Path, marker: &str) {
        write_atomic(&state.join(marker), b"ok\n");
    }

    fn stats_json(node: &MeshNode) -> serde_json::Value {
        let s = node.traversal_stats();
        serde_json::json!({
            "punches_attempted": s.punches_attempted,
            "punches_succeeded": s.punches_succeeded,
            "punches_failed": s.punches_failed,
            "relay_fallbacks": s.relay_fallbacks,
            "punch_timeouts": s.punch_timeouts,
            "punch_rejections": s.punch_rejections,
            "rendezvous_no_relay": s.rendezvous_no_relay,
            "upgrades_attempted": s.upgrades_attempted,
            "upgrades_succeeded": s.upgrades_succeeded,
            "upgrades_deferred_busy": s.upgrades_deferred_busy,
            "port_mapping_active": s.port_mapping_active,
            "port_mapping_renewals": s.port_mapping_renewals,
        })
    }

    async fn serve_forever() -> ! {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    /// `keygen`: print `{seed_hex, node_id}` for a fresh identity. The
    /// node_id is what `MeshNode` derives from the keypair — obtained by
    /// constructing a throwaway node on a loopback ephemeral port.
    async fn run_keygen() {
        let seed = random_seed();
        let keypair = EntityKeypair::from_bytes(seed);
        let cfg = node_config("127.0.0.1:0".parse().unwrap(), false);
        let node = MeshNode::new(keypair, cfg).await.expect("keygen node");
        println!(
            "{}",
            serde_json::json!({
                "seed_hex": hex::encode(seed),
                "node_id": node.node_id(),
            })
        );
    }

    async fn run_public(flags: HashMap<String, String>) {
        let name = flags.get("name").cloned().unwrap_or_else(|| usage());
        let bind: SocketAddr = flags
            .get("bind")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| usage());
        let state = PathBuf::from(flags.get("state").cloned().unwrap_or_else(|| usage()));
        let joiners: Vec<String> = flags
            .get("joiners")
            .map(|s| s.split(',').map(str::to_string).collect())
            .unwrap_or_default();

        let node = Arc::new(
            MeshNode::new(keypair_from(&flags), node_config(bind, false))
                .await
                .expect("public node"),
        );

        // Optional pre-start dial to another public (e.g. R → X) so the
        // publics have ≥2 peers for their own classification once the
        // joiners land. The dialed public must list us in its accept
        // order first.
        let connect_to = flags.get("connect-to").cloned();

        write_info(
            &state,
            &NodeInfo {
                name: name.clone(),
                node_id: node.node_id(),
                pubkey_hex: hex::encode(node.public_key()),
                addr: bind.to_string(),
            },
        );

        if let Some(peer) = &connect_to {
            let info = wait_for_info(&state, peer).await;
            wait_for_marker(&state, &format!("{peer}_accept_{name}")).await;
            let pk_bytes = hex::decode(&info.pubkey_hex).unwrap();
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&pk_bytes);
            node.connect(info.addr.parse().unwrap(), &pk, info.node_id)
                .await
                .expect("public connect to peer public");
        }

        // Accept each expected peer in file-coordinated order —
        // `accept(node_id)` assigns the given id to whoever completes
        // the handshake, so exactly one dialer may be in flight per turn.
        for j in &joiners {
            let info = wait_for_info(&state, j).await;
            write_marker(&state, &format!("{name}_accept_{j}"));
            node.accept(info.node_id).await.expect("accept joiner");
        }

        node.start_arc();
        node.reclassify_nat().await;
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("public announce");
        write_marker(&state, &format!("{name}_started"));
        serve_forever().await
    }

    async fn run_joiner(flags: HashMap<String, String>) {
        let name = flags.get("name").cloned().unwrap_or_else(|| usage());
        let bind: SocketAddr = flags
            .get("bind")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| usage());
        let state = PathBuf::from(flags.get("state").cloned().unwrap_or_else(|| usage()));
        let publics: Vec<String> = flags
            .get("publics")
            .map(|s| s.split(',').map(str::to_string).collect())
            .unwrap_or_default();
        // A joiner without publics can't dial, classify, or (in
        // upgrade mode) name a relay — `public_infos[0]` below would
        // panic on an empty list (cubic P2). Fail the configuration
        // loudly instead.
        if publics.is_empty() {
            eprintln!("natsim_node: joiner requires --publics with at least one public node");
            std::process::exit(2);
        }
        let auto_upgrade = flags.contains_key("auto-upgrade");
        let target = flags.get("target").cloned();
        let mode = flags.get("mode").cloned().unwrap_or_else(|| "wait".into());

        let node = Arc::new(
            MeshNode::new(keypair_from(&flags), node_config(bind, auto_upgrade))
                .await
                .expect("joiner node"),
        );

        write_info(
            &state,
            &NodeInfo {
                name: name.clone(),
                node_id: node.node_id(),
                pubkey_hex: hex::encode(node.public_key()),
                addr: bind.to_string(),
            },
        );

        // Dial every public in its accept-turn.
        let mut public_infos: Vec<NodeInfo> = Vec::new();
        for p in &publics {
            let info = wait_for_info(&state, p).await;
            wait_for_marker(&state, &format!("{p}_accept_{name}")).await;
            let pk_bytes = hex::decode(&info.pubkey_hex).unwrap();
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&pk_bytes);
            node.connect(info.addr.parse().unwrap(), &pk, info.node_id)
                .await
                .expect("joiner connect to public");
            public_infos.push(info);
        }

        node.start_arc();
        // Classify against the two publics (distinct public IPs → real
        // cone-vs-symmetric discrimination), then announce class+reflex.
        //
        // Retry the sweep until a concrete class lands (or the
        // deadline). A single post-connect sweep can lose its reflex
        // probes to a session that's still warming right after the
        // handshake; the <2-observation guard then keeps the prior
        // class (Unknown) rather than flapping, and the node announces
        // `nat:unknown` for the rest of the run. The loopback classify
        // suites poll the same way. NOTE: this only rescues a class
        // that would otherwise arrive late — it can't correct a sweep
        // that observes the wrong reflex (e.g. a mis-simulated NAT).
        {
            use net::adapter::net::traversal::classify::NatClass;
            let deadline = tokio::time::Instant::now() + CLASSIFY_TIMEOUT;
            loop {
                node.reclassify_nat().await;
                if node.nat_class() != NatClass::Unknown {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    eprintln!(
                        "natsim_node: {name} NAT class stayed Unknown after \
                         {CLASSIFY_TIMEOUT:?}; proceeding (outcome will report Unknown)"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("joiner announce");
        write_marker(&state, &format!("{name}_ready"));

        let Some(target) = target else {
            // Responder: serve until the script kills us.
            serve_forever().await;
        };

        // Initiator: wait for the target's identity + readiness, then
        // for its announcement (class + reflex) to propagate into our
        // own index — the same visibility gate the loopback suites use.
        let tinfo = wait_for_info(&state, &target).await;
        wait_for_marker(&state, &format!("{target}_ready")).await;
        let t_pk = {
            let bytes = hex::decode(&tinfo.pubkey_hex).unwrap();
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&bytes);
            pk
        };
        let visible_deadline = tokio::time::Instant::now() + COORD_TIMEOUT;
        while node.peer_reflex_addr(tinfo.node_id).is_none() {
            if tokio::time::Instant::now() > visible_deadline {
                eprintln!("natsim_node: target reflex never became visible");
                std::process::exit(3);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let outcome = match mode.as_str() {
            "punch" => {
                let started = tokio::time::Instant::now();
                let result = node.connect_direct_auto(tinfo.node_id, &t_pk).await;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                serde_json::json!({
                    "mode": "punch",
                    "ok": result.is_ok(),
                    "err_kind": result.as_ref().err().map(|e| e.kind()),
                    "elapsed_ms": elapsed_ms,
                    "session_addr": node.peer_addr(tinfo.node_id).map(|a| a.to_string()),
                    "self_nat_class": format!("{:?}", node.nat_class()),
                    "peer_nat_class": format!("{:?}", node.peer_nat_class(tinfo.node_id)),
                    "stats": stats_json(&node),
                })
            }
            "upgrade" => {
                // Establish a deliberately relay-routed session through
                // the first public, then wait for the background upgrade
                // to migrate it off the relay.
                let relay_addr: SocketAddr = public_infos[0].addr.parse().unwrap();
                let started = tokio::time::Instant::now();
                let connected = node
                    .connect_via(relay_addr, &t_pk, tinfo.node_id)
                    .await
                    .is_ok();
                let on_relay = node.peer_addr(tinfo.node_id) == Some(relay_addr);
                let mut upgraded = false;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                while tokio::time::Instant::now() < deadline {
                    if connected && node.peer_addr(tinfo.node_id) != Some(relay_addr) {
                        upgraded = true;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                serde_json::json!({
                    "mode": "upgrade",
                    "ok": connected,
                    "started_on_relay": on_relay,
                    "upgraded": upgraded,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "session_addr": node.peer_addr(tinfo.node_id).map(|a| a.to_string()),
                    "relay_addr": relay_addr.to_string(),
                    "self_nat_class": format!("{:?}", node.nat_class()),
                    "stats": stats_json(&node),
                })
            }
            other => {
                eprintln!("natsim_node: unknown mode {other}");
                std::process::exit(2);
            }
        };

        write_atomic(
            &state.join(format!("{name}_outcome.json")),
            &serde_json::to_vec_pretty(&outcome).unwrap(),
        );
        // Stay alive briefly so the just-established session (and the
        // counterpart's view of it) isn't torn down before the script
        // collects verdicts.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    pub fn main() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let args: Vec<String> = std::env::args().skip(1).collect();
            let Some(role) = args.first() else { usage() };
            let flags = parse_flags(&args[1..]);
            match role.as_str() {
                "keygen" => run_keygen().await,
                "public" => run_public(flags).await,
                "joiner" => run_joiner(flags).await,
                _ => usage(),
            }
        });
    }
} // mod natsim

#[cfg(all(feature = "net", feature = "nat-traversal"))]
fn main() {
    natsim::main();
}
