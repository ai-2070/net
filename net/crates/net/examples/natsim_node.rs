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
//!   announces, and (optionally) drives a punch / upgrade / RTC
//!   upgrade toward a target joiner, writing the outcome.
//! - `capabilities` — print which optional cargo features this
//!   binary was built with, so a scenario that needs one can refuse
//!   before provisioning anything.
//!
//! RTC (`webrtc` feature): a joiner given `--rtc-bind` (and, behind
//! a NAT, `--rtc-public`) runs a second, dedicated RTC socket and
//! announces `rtc_addr`. `--mode rtc` additionally drives the
//! client half: a relay-routed session, then `offer_direct_path`
//! until the peer sits on a `PeerAddr::Rtc` DataChannel.
//!
//! Build: `cargo build --example natsim_node --features net,nat-traversal`
//!        (add `webrtc` for the RTC scenarios)
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
    use std::net::{IpAddr, SocketAddr};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use net::adapter::net::behavior::capability::CapabilitySet;
    use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

    const PSK: [u8; 32] = [0x42u8; 32];

    /// The result of one unsolicited STUN binding request (R8).
    #[derive(Default)]
    struct StunProbe {
        ok: bool,
        target: Option<String>,
        mapped: Option<String>,
        /// The probe socket's OWN tuple, as the kernel bound it.
        ///
        /// Recorded so the row can assert the reply against a value
        /// this process observed rather than against a hardcoded
        /// address: on this topology the client is un-NAT'd
        /// (`self_nat_class: Open`), so a faithful
        /// XOR-MAPPED-ADDRESS must equal this exactly. A reply that
        /// names any other tuple is describing a different socket —
        /// which is the defect this field exists to catch.
        local: Option<String>,
    }

    /// Send ONE RFC 5389 binding request to `target` **from `local`**,
    /// and read the response's XOR-MAPPED-ADDRESS.
    ///
    /// Deliberately not an ICE check: a check carries `USERNAME`,
    /// belongs to a session, and never reaches the anchor's bare
    /// responder — so it could not show that the ADVERTISED address
    /// is the one being aimed at.
    ///
    /// # Why `local` is a parameter and not `0.0.0.0`
    ///
    /// The reply's XOR-MAPPED-ADDRESS is a statement about **the
    /// source tuple the request actually arrived with**, so it says
    /// something about THIS node only if the request left from this
    /// node's own address. A wildcard bind delegates that choice to
    /// the kernel's source-address selection, which picks the
    /// outgoing device's PRIMARY address — and `setup.sh` puts four
    /// addresses on `nsim_wan`'s `br0` (`10.99.0.1` first, then
    /// `.10`, `.11`, and `.12` under `--public-b`). A public joiner
    /// bound to `10.99.0.12` therefore probed from `10.99.0.1` and
    /// was told, correctly, about a mapping that was not its own:
    /// the reply was a statement about a local socket, which is
    /// precisely what this probe exists not to be. Binding `local`
    /// makes the reply about the node.
    #[cfg(feature = "webrtc")]
    async fn stun_probe(target: &str, local: IpAddr) -> StunProbe {
        use net::adapter::net::rtc::{parse_xor_mapped_address, STUN_MAGIC_COOKIE};

        let mut probe = StunProbe {
            target: Some(target.to_string()),
            ..StunProbe::default()
        };
        let Ok(addr) = target.parse::<SocketAddr>() else {
            return probe;
        };
        // This node's own address, ephemeral port. A failed bind is
        // NOT silently downgraded to a wildcard one: a probe that
        // could not use this node's address cannot make a statement
        // about this node's mapping, so `ok: false` is the honest
        // outcome and the row fails naming it.
        let Ok(socket) = tokio::net::UdpSocket::bind(SocketAddr::new(local, 0)).await else {
            return probe;
        };
        // Read back from the socket, not assembled from `local` and a
        // guess: the port is the kernel's choice and the row compares
        // the anchor's reply against it.
        probe.local = socket.local_addr().ok().map(|a| a.to_string());
        // A binding request is 20 bytes: type, length, cookie, id.
        let mut request = Vec::with_capacity(20);
        request.extend_from_slice(&0x0001u16.to_be_bytes());
        request.extend_from_slice(&0u16.to_be_bytes());
        request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        request.extend_from_slice(&[0x2Au8; 12]);
        for _ in 0..5 {
            if socket.send_to(&request, addr).await.is_err() {
                continue;
            }
            let mut buf = [0u8; 512];
            match tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) if from == addr => {
                    if let Some(mapped) = parse_xor_mapped_address(&buf[..n]) {
                        probe.ok = true;
                        probe.mapped = Some(mapped.to_string());
                        return probe;
                    }
                }
                _ => {}
            }
        }
        probe
    }

    #[cfg(not(feature = "webrtc"))]
    async fn stun_probe(_target: &str, _local: IpAddr) -> StunProbe {
        StunProbe::default()
    }
    /// How long coordination waits (files, reflex visibility) may take.
    const COORD_TIMEOUT: Duration = Duration::from_secs(60);
    // How long a joiner keeps re-running the classification sweep
    // waiting for a concrete NAT class before giving up and proceeding.
    const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);

    fn usage() -> ! {
        eprintln!(
            "usage:\n  natsim_node keygen\n  natsim_node capabilities\n  \
         natsim_node public --name N --bind IP:PORT \
         --state DIR --joiners a,b [--connect-to x]\n  natsim_node joiner --name N \
         --bind IP:PORT --state DIR --publics r,x [--seed-hex H] [--auto-upgrade] \
         [--rtc-bind IP:PORT] [--rtc-public IP:PORT] \
         [--target N --mode punch|upgrade|rtc] "
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
        // Set both ways, never `if auto_upgrade`: the flag defaults on,
        // so leaving it unset would silently enable the upgrade in the
        // punch / fallback / skip scenarios that deliberately omit
        // `--auto-upgrade` and assert on un-upgraded behavior.
        cfg = cfg.with_auto_direct_upgrade(auto_upgrade);
        cfg
    }

    /// The RTC half of a joiner's config, built from `--rtc-bind` /
    /// `--rtc-public`. `None` when neither is given, which is every
    /// pre-Stage-4 scenario — a node with `rtc: None` announces none
    /// of the RTC fields and behaves exactly as before.
    ///
    /// `--rtc-public` is what a NAT'd anchor is told about its own
    /// mapping: the driver advertises it as the host candidate and
    /// the mesh announces it as `rtc_addr`. A node that is not given
    /// one does not guess, so a public client simply omits it.
    #[cfg(feature = "webrtc")]
    fn rtc_config_from(
        flags: &HashMap<String, String>,
    ) -> Option<net::adapter::net::rtc::RtcConfig> {
        use net::adapter::net::rtc::RtcConfig;
        let bind: Option<SocketAddr> = flags.get("rtc-bind").map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("--rtc-bind must be IP:PORT, got {s:?}"))
        });
        let public: Option<SocketAddr> = flags.get("rtc-public").map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("--rtc-public must be IP:PORT, got {s:?}"))
        });
        if bind.is_none() && public.is_none() {
            return None;
        }
        let mut cfg = RtcConfig::new();
        cfg.bind_addr = bind;
        cfg.public_addr = public;
        // An anchor publishes an `rtc_addr` so peers can aim at it;
        // answering a bare binding request on that socket is part of
        // what publishing it means, and the responder is off by
        // default (R8: the probe's refusal was over-determined —
        // the responder was disabled AND the gateway drops
        // unsolicited inbound; only the second is by design).
        cfg.serve_stun = true;
        // **The SECOND announced endpoint (Stage 6 §6.12.2).**
        //
        // A separate UDP socket that serves STUN and is never an ICE
        // peer, announced as `rtc_stun_addr`. Two flags, not one
        // derived port: `--stun-bind` is where the socket sits and
        // `--stun-public` is what a NAT'd anchor is told its mapping
        // is — the same split `--rtc-bind` / `--rtc-public` has, and
        // for the same reason. A node given neither announces no
        // second endpoint and behaves exactly as before.
        cfg.stun_addr = flags.get("stun-bind").map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("--stun-bind must be IP:PORT, got {s:?}"))
        });
        cfg.stun_public_addr = flags.get("stun-public").map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("--stun-public must be IP:PORT, got {s:?}"))
        });
        // A real network between the endpoints, not loopback: give ICE
        // room for the restricted-cone pinhole to open (the anchor's
        // first outbound check is what makes the client's checks
        // deliverable), while staying well inside the scenario's own
        // 120 s verdict budget.
        cfg.ice_deadline = Duration::from_secs(15);
        Some(cfg)
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct NodeInfo {
        name: String,
        node_id: u64,
        pubkey_hex: String,
        addr: String,
        /// The `rtc_addr` this node actually **announced**, read back
        /// from its own emitted `CapabilityAnnouncement` once it has
        /// announced. Written by the RTC roles only; `#[serde(default)]`
        /// so every existing scenario's info file still parses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rtc_addr: Option<String>,
        /// The `rtc_stun_addr` this node actually **announced** — the
        /// separate STUN endpoint of Stage 6 §6.12.2, read back out
        /// of the same emitted announcement rather than echoed from
        /// `--stun-public`. A peer that wants to use the anchor as a
        /// STUN server learns it from here, which is the only place
        /// the product puts it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rtc_stun_addr: Option<String>,
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
        // Make it world-readable *before* the rename. The helpers run as
        // root inside the namespaces, so a fresh file lands 0600 under
        // the default umask, while the non-root `cargo test` wrapper is
        // what has to read these artifacts. `run_scenario.sh` chmods the
        // state dir at the end, but the stats snapshots keep being
        // rewritten every second afterwards — each rename installing a
        // new root-only inode over the relaxed one — so a post-hoc chmod
        // races the writers. Setting the mode here is the only version
        // that holds for a file still being updated.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
        }
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

    /// The RTC counters, for the scenarios that configure an RTC
    /// socket. Nested under `stats.rtc` rather than flattened so the
    /// traversal keys every other scenario asserts on keep their
    /// exact shape.
    #[cfg(feature = "webrtc")]
    fn rtc_stats_json(node: &MeshNode) -> serde_json::Value {
        let s = node.rtc_stats();
        serde_json::json!({
            "ice_attempted": s.ice_attempted(),
            "ice_direct": s.ice_direct(),
            "ice_relayed": s.ice_relayed(),
            // Which side lost a signalling frame, if one was lost:
            // `delivered` is this node's own handler, `forwarded` is
            // the relay leg, and the two refusal counters name a
            // cause instead of a silent drop.
            "signal_delivered": s.signal_delivered(),
            "signal_forwarded": s.signal_forwarded(),
            "signal_malformed": s.signal_malformed(),
            "signal_over_budget": s.signal_over_budget(),
            "signal_engine_full": s.signal_engine_full(),
            "signal_unknown_dialog": s.signal_unknown_dialog(),
            // R8: unsolicited binding requests this node's own STUN
            // responder answered — a peer aiming at the address we
            // published, which an ICE check could never demonstrate.
            "stun_binding_requests": s.stun_binding_requests(),
        })
    }

    fn stats_json(node: &MeshNode) -> serde_json::Value {
        let s = node.traversal_stats();
        #[cfg_attr(
            not(feature = "webrtc"),
            expect(unused_mut, reason = "the RTC block is the only mutation")
        )]
        let mut out = serde_json::json!({
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
        });
        // Only for a node that actually runs an RTC driver: a key
        // that is always present but always zero reads as "RTC did
        // nothing" on nodes that never had RTC at all.
        #[cfg(feature = "webrtc")]
        if node.rtc_driver().is_some() {
            out["rtc"] = rtc_stats_json(node);
        }
        out
    }

    /// Park for the rest of the scenario, republishing this node's
    /// traversal stats to `<name>_stats.json` once a second.
    ///
    /// Only the initiator writes an `outcome.json`, so every other
    /// node's view of the punch used to be invisible: when A reports
    /// `punch_timeouts: 1` there was no way to tell whether the
    /// responder ever received the introduce, armed an observer, or
    /// emitted an ack. These snapshots make the responder's and
    /// coordinator's counters readable after the fact, which is what
    /// separates "R never fanned out" from "B dropped the introduce"
    /// from "B's observer never fired".
    async fn serve_forever_publishing_stats(
        node: Arc<MeshNode>,
        state: PathBuf,
        name: String,
    ) -> ! {
        let path = state.join(format!("{name}_stats.json"));
        loop {
            write_atomic(
                &path,
                &serde_json::to_vec_pretty(&stats_json(&node)).unwrap(),
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
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
                rtc_addr: None,
                rtc_stun_addr: None,
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
        serve_forever_publishing_stats(node, state, name).await
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
        // Fail the configuration loudly rather than silently running a
        // node with no RTC socket: without the feature the flags below
        // are accepted by `parse_flags` and then do nothing, and the
        // scenario would fail much later as "offer_direct_path: rtc is
        // not configured". `run_scenario.sh` pre-flights the same fact
        // via the `capabilities` role before touching a namespace.
        #[cfg(not(feature = "webrtc"))]
        if flags.contains_key("rtc-bind") || flags.contains_key("rtc-public") || mode == "rtc" {
            eprintln!(
                "natsim_node: --rtc-bind/--rtc-public/--mode rtc need the `webrtc` \
                 cargo feature (rebuild with --features net,nat-traversal,webrtc)"
            );
            std::process::exit(2);
        }

        #[cfg_attr(
            not(feature = "webrtc"),
            expect(unused_mut, reason = "the RTC half is the only mutation")
        )]
        let mut cfg = node_config(bind, auto_upgrade);
        #[cfg(feature = "webrtc")]
        let rtc_enabled = {
            let rtc = rtc_config_from(&flags);
            let enabled = rtc.is_some();
            cfg.rtc = rtc;
            enabled
        };
        let node = Arc::new(
            MeshNode::new(keypair_from(&flags), cfg)
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
                rtc_addr: None,
                rtc_stun_addr: None,
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
            // A concrete class needs ≥2 distinct observers (the sweep's
            // <2-observation guard). With fewer publics every sweep can
            // only re-confirm Unknown, so don't burn the retry budget —
            // sweep once and proceed.
            if publics.len() >= 2 {
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
            } else {
                node.reclassify_nat().await;
            }
        }
        node.announce_capabilities(CapabilitySet::new())
            .await
            .expect("joiner announce");
        // Republish this node's identity with the `rtc_addr` it
        // ACTUALLY announced, read back out of its own emitted
        // `CapabilityAnnouncement` rather than echoed from the flag.
        // That is the fact the scenario is about: a NAT'd anchor must
        // put its *mapped* address on the wire, not the private
        // address its RTC socket is bound to. The rewrite lands before
        // the `_ready` marker below, so a peer that waits for the
        // marker and then re-reads the info file cannot observe the
        // pre-announce version.
        #[cfg(feature = "webrtc")]
        if rtc_enabled {
            // BOTH announced endpoints, read out of the SAME emitted
            // announcement. `rtc_stun_addr` is the second one
            // (§6.12.2); taking it from the announcement rather than
            // from `--stun-public` is what makes the leg able to
            // assert the anchor announced its MAPPED address and not
            // the private socket it binds.
            let ann = node.local_announcement_for_test();
            let announced = ann.as_ref().and_then(|a| a.rtc_addr).map(|a| a.to_string());
            let announced_stun = ann.as_ref().and_then(|a| a.rtc_stun_addr.clone());
            write_info(
                &state,
                &NodeInfo {
                    name: name.clone(),
                    node_id: node.node_id(),
                    pubkey_hex: hex::encode(node.public_key()),
                    addr: bind.to_string(),
                    rtc_addr: announced,
                    rtc_stun_addr: announced_stun,
                },
            );
        }
        write_marker(&state, &format!("{name}_ready"));

        let Some(target) = target else {
            // Responder: serve until the script kills us. Its stats are
            // the ones that say whether the introduce ever landed.
            serve_forever_publishing_stats(node, state, name).await;
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
                    // Reflex A used to reach B — if this isn't B's
                    // gateway public (10.99.0.x:700x), the punch train
                    // fired at the wrong address and could never land.
                    "peer_reflex": node.peer_reflex_addr(tinfo.node_id).map(|a| a.to_string()),
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
                    // Diagnostics for an `upgrades_attempted=0` outcome —
                    // pinpoint which early-return attempt_direct_upgrade
                    // takes. `peer_nat_class=Unknown` ⇒ pair_action lands
                    // on SinglePunch (defers); `peer_reflex=null` ⇒
                    // Direct-with-no-reflex (records failure);
                    // `upgrade_loop_candidate=false` ⇒ the loop never
                    // considers the peer (C1 / relayed / throttle gate).
                    "peer_nat_class": format!("{:?}", node.peer_nat_class(tinfo.node_id)),
                    "peer_reflex": node.peer_reflex_addr(tinfo.node_id).map(|a| a.to_string()),
                    "upgrade_loop_candidate": node
                        .upgrade_is_loop_candidate_for_test(tinfo.node_id),
                    "stats": stats_json(&node),
                })
            }
            // The client half of the RTC-anchor scenario: a public
            // native node outside the NAT, reaching a NAT'd anchor
            // over a DataChannel. Signalling rides the relay-routed
            // session (`0x0D02`); the only address the client is ever
            // told for the anchor's RTC socket is the mapped one the
            // anchor advertises, so an installed `PeerAddr::Rtc`
            // endpoint IS the proof that the published `rtc_addr`
            // works through the NAT.
            #[cfg(feature = "webrtc")]
            "rtc" => {
                use net::adapter::net::PeerAddr;
                let relay_addr: SocketAddr = public_infos[0].addr.parse().unwrap();
                let started = tokio::time::Instant::now();
                let connected = node
                    .connect_via(relay_addr, &t_pk, tinfo.node_id)
                    .await
                    .is_ok();
                let on_relay = node.peer_addr(tinfo.node_id) == Some(relay_addr);

                // §5 Layer 1: the anchor's Noise static arrives in its
                // signed announcement, and the offerer's half of the
                // install handshakes against exactly that key. Waiting
                // for it here keeps an announcement that has not
                // propagated yet from being reported as an ICE failure.
                let key_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                while node.peer_announced_noise_pubkey(tinfo.node_id) != Some(t_pk)
                    && tokio::time::Instant::now() < key_deadline
                {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                let learned_noise_key =
                    node.peer_announced_noise_pubkey(tinfo.node_id) == Some(t_pk);

                // The anchor rewrote its info file with the two
                // endpoints it announced before it wrote
                // `<target>_ready`, which this initiator already
                // awaited — so this read cannot observe the
                // pre-announce version.
                let anchor_info = wait_for_info(&state, &target).await;
                let anchor_rtc_addr = anchor_info.rtc_addr;
                let anchor_stun_addr = anchor_info.rtc_stun_addr;

                // **The SECOND announced endpoint, probed for real
                // (Stage 6 §6.12.2).**
                //
                // One unsolicited binding request from a fresh socket
                // bound to THIS NODE'S OWN address (`bind.ip()`, the
                // address its mesh socket uses), aimed at
                // `rtc_stun_addr` — the address the
                // ANCHOR ANNOUNCED, not a flag this process was
                // given. Unlike the `rtc_addr` probe below, this one
                // is expected to be ANSWERED: an ICE socket behind an
                // address-restricted cone is reachable only after its
                // own outbound opens the mapping, while a STUN
                // endpoint that drops a stranger's first request
                // could never serve the peers it is announced to, so
                // `setup.sh --stun-port-a` forwards that single port.
                //
                // What the reply establishes is the part local sockets
                // cannot: the request crossed the gateway to a
                // private socket, was served, and the response came
                // back — and its XOR-MAPPED-ADDRESS is this client's
                // own public tuple as the ANCHOR saw it. Two
                // externally reachable mappings on one NAT'd anchor,
                // each observed with its own reply. The source
                // address is pinned rather than left to the kernel
                // because otherwise the reply describes whichever of
                // `br0`'s four addresses won source selection, not
                // this node — see `stun_probe`.
                //
                // Probed BEFORE the binding below, which shadows the
                // function's own name.
                let stun_endpoint_probe = match anchor_stun_addr.as_deref() {
                    Some(addr) => stun_probe(addr, bind.ip()).await,
                    None => StunProbe::default(),
                };

                // **R8, evidence half.** One unsolicited binding
                // request aimed at the announced RTC address, from a
                // FRESH socket bound to this node's own address for
                // the same reason as above. Under this topology it is
                // expected to be dropped, and that is worth recording
                // rather than asserting: the cone gateway is
                // address-restricted
                // by construction (`setup.sh` installs
                // `iifname gw?-wan udp dport <rtc> ct state new drop`
                // precisely so the scenario models a restricted NAT
                // and not a full-cone one). An anchor behind such a
                // NAT is reachable only after its own outbound check
                // opens the mapping — so "a stranger can use it as a
                // STUN server" is false here BY DESIGN, and asserting
                // it would be asserting the wrong topology.
                let stun_probe = match anchor_rtc_addr.as_deref() {
                    Some(addr) => stun_probe(addr, bind.ip()).await,
                    None => StunProbe::default(),
                };

                // Offer, then let the dialog's own completion owner
                // carry it into the fenced install. Retried, not raced:
                // the §12/C3 quiescence gate can refuse the first
                // replacement while the fresh routed session still has
                // unacked frames, and an attempt that loses ICE is one
                // attempt, not a verdict. The budget is sized so the
                // FAILING path still writes a verdict inside
                // `run_scenario.sh`'s 120 s wait: two 20 s attempts
                // behind the 15 s key wait, plus the classification
                // sweep before all of it. A scenario that times out
                // with no outcome file reports nothing but a tail of
                // logs; one that writes `transport: "udp"` names what
                // happened.
                let mut offers = 0u32;
                let mut on_rtc = false;
                let attempts_until = tokio::time::Instant::now() + Duration::from_secs(45);
                while connected && !on_rtc && tokio::time::Instant::now() < attempts_until {
                    if node.offer_direct_path(tinfo.node_id).await.is_ok() {
                        offers += 1;
                    }
                    // One `ice_deadline` (15 s) plus the Noise install
                    // and the announcement round trip behind it.
                    let settle_by = tokio::time::Instant::now() + Duration::from_secs(20);
                    while tokio::time::Instant::now() < settle_by {
                        if matches!(node.peer_endpoint(tinfo.node_id), Some(PeerAddr::Rtc(_))) {
                            on_rtc = true;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
                #[cfg(feature = "webrtc")]
                let selected = node.rtc_selected_pair(tinfo.node_id).await;
                #[cfg(not(feature = "webrtc"))]
                let selected: Option<(SocketAddr, SocketAddr, &'static str)> = None;
                serde_json::json!({
                    "mode": "rtc",
                    "ok": connected,
                    "started_on_relay": on_relay,
                    // The whole verdict in one field: `rtc` means the
                    // session sits on a DataChannel, `udp` means it is
                    // still on the relay (or a punched path).
                    "transport": match node.peer_endpoint(tinfo.node_id) {
                        Some(PeerAddr::Rtc(_)) => "rtc",
                        Some(_) => "udp",
                        None => "none",
                    },
                    "direct": node.peer_is_direct(tinfo.node_id),
                    "upgraded": on_rtc,
                    "offers": offers,
                    "learned_noise_key": learned_noise_key,
                    // What the anchor put on the wire. Behind a NAT
                    // this MUST be the gateway's mapped address, never
                    // the private address its RTC socket is bound to.
                    "anchor_rtc_addr": anchor_rtc_addr,
                    // R8: did an unsolicited binding request to THAT
                    // address get a well-formed success response, and
                    // what did it say our mapped address was?
                    "stun_probe_ok": stun_probe.ok,
                    "stun_probe_note": "unsolicited inbound is dropped by the \
                                        address-restricted cone gateway by design; \
                                        evidence, not a verdict",
                    "stun_probe_target": stun_probe.target,
                    "stun_probe_mapped": stun_probe.mapped,
                    // **The second announced endpoint (§6.12.2).**
                    // The address the anchor ANNOUNCED for STUN, and
                    // whether a stranger's binding request to it was
                    // answered — which is the only way a mapping is
                    // observed from outside rather than asserted from
                    // inside. `stun_endpoint_mapped` is this client's
                    // own public tuple as the anchor saw it, and
                    // `stun_endpoint_local` is the same tuple as THIS
                    // process bound it — the client is un-NAT'd here,
                    // so a faithful reply must equal it exactly, and
                    // the row asserts that rather than a prefix.
                    "anchor_stun_addr": anchor_stun_addr,
                    "stun_endpoint_probe_ok": stun_endpoint_probe.ok,
                    "stun_endpoint_target": stun_endpoint_probe.target,
                    "stun_endpoint_mapped": stun_endpoint_probe.mapped,
                    "stun_endpoint_local": stun_endpoint_probe.local,
                    // **R8, verdict half.** The address the client's
                    // ICE stack is actually transmitting to, and
                    // WHERE IT CAME FROM. `signalled` means it was
                    // learned from the anchor's announced candidate;
                    // `peer-reflexive` means it was discovered from
                    // the anchor's own inbound check and the
                    // announcement contributed nothing.
                    "selected_local": selected.as_ref().map(|(l, _, _)| l.to_string()),
                    "selected_remote": selected.as_ref().map(|(_, r, _)| r.to_string()),
                    "selected_learned": selected.as_ref().map(|(_, _, k)| *k),
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "session_addr": node.peer_addr(tinfo.node_id).map(|a| a.to_string()),
                    "relay_addr": relay_addr.to_string(),
                    "self_nat_class": format!("{:?}", node.nat_class()),
                    "peer_nat_class": format!("{:?}", node.peer_nat_class(tinfo.node_id)),
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

    /// Install a `RUST_LOG`-driven subscriber writing to **stderr**.
    ///
    /// Without this the crate's `tracing` calls go nowhere, so the
    /// rendezvous drop paths — the coordinator's fan-out checks and the
    /// responder's `unsolicited_introduce_permitted` gate, all of which
    /// drop silently by design — are invisible no matter what `RUST_LOG`
    /// says. They are the difference between "R never introduced B" and
    /// "B refused the introduce".
    ///
    /// stderr specifically: `keygen` prints its JSON to stdout and
    /// `run_scenario.sh` parses that with `sed`, so log lines must not
    /// share the stream.
    fn init_tracing() {
        use tracing_subscriber::{fmt, EnvFilter};
        let raw = std::env::var("RUST_LOG").unwrap_or_default();
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
        let rendered = filter.to_string();
        let _ = fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .try_init();
        // Announce the filter actually in force, and prove the enabled
        // level by emitting one line at each of debug and trace.
        // Otherwise a log with no TRACE lines is ambiguous between "the
        // filter was too coarse" and "no trace-level code path ran" —
        // an ambiguity that cost a CI round here.
        eprintln!("natsim_node: RUST_LOG={raw:?} effective_filter={rendered:?}");
        // Emit under the *library's* target prefix, not this example's.
        // A filter like `net::adapter::net=trace` doesn't match
        // `natsim_node`, so self-test lines logged under the default
        // target would stay silent even when the mesh code is at trace
        // — proving nothing. These two say exactly which levels are
        // live for the target the rendezvous code logs under.
        tracing::debug!(target: "net::adapter::net::selftest", "natsim_node: debug enabled");
        tracing::trace!(target: "net::adapter::net::selftest", "natsim_node: trace enabled");
    }

    /// `capabilities`: which optional features this binary carries.
    ///
    /// A scenario that needs one (today: `webrtc`) pre-flights it and
    /// refuses before a single namespace is provisioned, instead of
    /// discovering the gap 120 s later as an empty outcome file. The
    /// no-feature build never reaches here — its stub `main` exits 2
    /// with its own message, which the same check catches.
    fn run_capabilities() {
        println!(
            "{}",
            serde_json::json!({
                "webrtc": cfg!(feature = "webrtc"),
                "nat_traversal": cfg!(feature = "nat-traversal"),
                "fixtures": cfg!(feature = "fixtures"),
            })
        );
    }

    pub fn main() {
        init_tracing();
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
                "capabilities" => run_capabilities(),
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
