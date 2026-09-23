//! Subnet join through the CLI (V3-2 S3) and the V3-4 removal journey end to
//! end: an operator's `up --enroll` holds only a delegated subnet issuer
//! (the root stays offline) and verifies admission; a device joins with a
//! subnet-scoped token, its joined `up` is admitted over the wire (proven by
//! the verifier's verdict), the operator removes it with `subnet remove`
//! (attested applied and persisted), and the device's next start is refused.
//! Real subprocesses on loopback with router mapping disabled.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

/// The mesh PSK: the operator reads it from a file so a second, in-process
/// verifier can share the mesh. The CLI `subnet remove` never sees it.
const PSK: [u8; 32] = [0x5A; 32];

struct Fx {
    tmp: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self { tmp }
    }

    fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }

    fn base(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"));
        c
    }

    /// One-shot command with `--output json --state-dir`, stdin optional,
    /// killed and reported if it is still running after 20 s.
    fn run_with(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = self
            .base()
            .args(["--output", "json"])
            .args(args)
            .arg("--state-dir")
            .arg(self.state())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        for _ in 0..400 {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        panic!(
            "{args:?} did not exit within 20s: {:?}",
            child.wait_with_output()
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, None)
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn up(&self, extra: &[&str]) -> Up {
        self.up_on("127.0.0.1:0", extra)
    }

    fn up_on(&self, bind: &str, extra: &[&str]) -> Up {
        let mut child = self
            .base()
            .args(["--output", "ndjson", "up", "--bind", bind])
            .args(extra)
            .arg("--state-dir")
            .arg(self.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = read_row(&mut stdout, &mut child);
        assert_eq!(ready["event"], "ready", "{ready}");
        Up { child, ready }
    }
}

struct Up {
    child: Child,
    ready: Value,
}

impl Drop for Up {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_row(stdout: &mut BufReader<ChildStdout>, child: &mut Child) -> Value {
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut line = String::new();
            let _ = stdout.read_line(&mut line);
            let _ = tx.send(line);
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) if !line.trim().is_empty() => serde_json::from_str(&line).unwrap(),
            _ => {
                let _ = child.kill();
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                panic!("no row from up; stderr: {err}");
            }
        }
    })
}

fn token_of(created: &Value) -> String {
    created["token"].as_str().unwrap().to_string()
}

fn keygen(dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let out = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["subnet", "keygen", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = std::fs::read_to_string(&key).unwrap();
    let hex = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("entity_id_hex"))
        .unwrap()
        .trim_start_matches(['=', ' '])
        .trim()
        .trim_matches('"')
        .to_string();
    (key, hex)
}

/// The offline subnet ceremony: a root key, an issuer key, and the
/// root-signed issuer grant over subnet `3` (ATTACH, ROUTE).
fn ceremony(dir: &Path) -> (PathBuf, String, PathBuf, PathBuf) {
    let (root, root_hex) = keygen(dir, "subnet-root.toml");
    let (issuer, issuer_hex) = keygen(dir, "subnet-issuer.toml");
    let grant = dir.join("issuer.grant");
    let out = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["subnet", "issue-issuer", "--root-key"])
        .arg(&root)
        .args(["--authority", &root_hex, "--issuer", &issuer_hex])
        .args(["--scope", "3", "--max-rights", "attach,route", "--out"])
        .arg(&grant)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    (root, root_hex, issuer, grant)
}

#[test]
fn a_device_joins_a_subnet_is_admitted_and_removal_takes_effect() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (root, root_hex, issuer, grant) = ceremony(&keys);
    let psk = keys.join("mesh.psk");
    std::fs::write(&psk, hex::encode(PSK)).unwrap();
    // `--psk-from file:` refuses a group- or world-readable secret on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&psk, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // A second enforcement point: a durable verifier for the same authority,
    // running in this process, connected to nobody yet.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let root_entity = net::adapter::net::identity::EntityId::from_bytes(
        hex::decode(&root_hex).unwrap().try_into().unwrap(),
    );
    let other = rt.block_on(async {
        let mut cfg = net::adapter::net::MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK)
            .with_heartbeat_interval(Duration::from_millis(500))
            .with_session_timeout(Duration::from_secs(5))
            .with_subnet_authority(net::adapter::net::subnet::SubnetAuthorityConfig {
                authority: root_entity.clone(),
                roots: vec![root_entity.clone()],
                maximum_grant_lifetime_secs: 30 * 24 * 60 * 60,
            })
            .with_subnet_floor_store(keys.join("other-floors"));
        cfg.socket_buffers = net::adapter::net::SocketBufferConfig {
            send_buffer_size: 256 * 1024,
            recv_buffer_size: 256 * 1024,
        };
        let node = std::sync::Arc::new(
            net::adapter::net::MeshNode::new(
                net::adapter::net::identity::EntityKeypair::generate(),
                cfg,
            )
            .await
            .unwrap(),
        );
        node.start();
        node
    });
    let _other_serving = rt.block_on(async { other.serve_subnet_floor_status().unwrap() });
    let other_contact = format!(
        "{}@{}#{}",
        hex::encode(other.entity_id().as_bytes()),
        other.local_addr(),
        hex::encode(other.public_key())
    );

    let op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--psk-from",
        &format!("file:{}", psk.display()),
        "--subnet-issuer-grant",
        grant.to_str().unwrap(),
        "--subnet-issuer-key",
        issuer.to_str().unwrap(),
    ]);
    let subnet = &op.ready["enrollment"]["subnet"];
    assert_eq!(subnet["verifier"], true, "{}", op.ready);
    assert_eq!(subnet["authority"], root_hex.as_str());

    // An offer outside the issuer grant is refused at creation.
    let refused = operator.run(&["invite", "create", "--subnet", "4.1"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("outside this node's issuer grant"));

    let created = operator.json(&["invite", "create", "--subnet", "3.7"]);
    assert_eq!(created["subnet"]["scope"], "3.7", "{created}");
    assert_eq!(created["subnet"]["rights"], "attach");

    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(joined["subnet"]["credentials"], "installed", "{joined}");
    let device = joined["device"].as_str().unwrap().to_string();

    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["admitted"], true, "{}", node.ready);
    assert_eq!(jsub["scope"], "3.7");
    drop(node);

    // Remove the device from 3.7 through the operator's own node: the PSK was
    // generated by `up` and never leaves it, the root key never reaches the
    // node, and the node's attestation is verified by the CLI.
    let removed = operator
        .base()
        .args(["--output", "json", "subnet", "remove", "--root-key"])
        .arg(&root)
        .args([
            "--authority",
            &root_hex,
            "--scope",
            "3.7",
            "--topology-epoch",
            "0",
        ])
        .args([
            "--revision",
            "1",
            "--subject",
            &device,
            "--minimum-generation",
            "2",
        ])
        .args([
            "--verifier",
            "self",
            "--verifier",
            &other_contact,
            "--state-dir",
        ])
        .arg(operator.state())
        .output()
        .unwrap();
    assert!(removed.status.success(), "{removed:?}");
    let removed: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed["verifiers"][0]["state"], "applied", "{removed}");
    // Forwarded by the operator's node to the other verifier, which it had
    // to connect to first; that verifier's own signature is what counts.
    assert_eq!(removed["verifiers"][1]["state"], "applied", "{removed}");
    assert_eq!(
        removed["verifiers"][1]["verifier"],
        hex::encode(other.entity_id().as_bytes())
    );
    assert_eq!(removed["complete"], true, "{removed}");

    // The device's next start is refused admission with its old credentials.
    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["admitted"], false, "{}", node.ready);
    assert!(
        jsub["detail"].as_str().unwrap().contains("revoked"),
        "{}",
        node.ready
    );
}

/// Leaf renewal (V3-2 task 5): the operator issues short-lived leaves; the
/// joined node renews its own in the background before expiry, renews an
/// already-expired one at start and is admitted with it, and once removed it
/// can no longer renew — renewal never re-admits a removed device.
#[test]
fn a_joined_node_renews_its_subnet_leaf_and_removal_stops_renewal() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (root, root_hex, issuer, grant) = ceremony(&keys);

    let _op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--subnet-issuer-grant",
        grant.to_str().unwrap(),
        "--subnet-issuer-key",
        issuer.to_str().unwrap(),
        "--subnet-leaf-ttl",
        "15s",
    ]);
    let created = operator.json(&["invite", "create", "--subnet", "3.7"]);
    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["subnet"]["credentials"], "installed", "{joined}");
    let device = joined["device"].as_str().unwrap().to_string();

    // Fresh leaf: admitted as issued, no renewal needed yet.
    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["admitted"], true, "{}", node.ready);
    assert_eq!(jsub["renewed"], false, "{}", node.ready);
    let first = jsub["expires_at"].as_u64().unwrap();

    // Background renewal moves the expiry forward while the node runs.
    let mut renewed = None;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        let status = agent.json(&["node", "status"]);
        if let Some(at) = status["subnet_expires_at"]
            .as_u64()
            .filter(|&at| at > first)
        {
            renewed = Some(at);
            break;
        }
    }
    let second = renewed.expect("the running node renewed its subnet leaf");
    drop(node);

    // Stopped past expiry: the next start renews first, then is admitted.
    // The renewal's nRPC streams leave the killed node's old session busy
    // at the operator, but the node no longer speaks on it, so its
    // re-handshake is not deferred for the whole session timeout.
    wait_past(second);
    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["renewed"], true, "{}", node.ready);
    assert_eq!(jsub["admitted"], true, "{}", node.ready);
    let third = jsub["expires_at"].as_u64().unwrap();
    drop(node);

    // Removed from 3.7 at the issuing node: renewal is refused as revoked.
    let removed = operator
        .base()
        .args(["--output", "json", "subnet", "remove", "--root-key"])
        .arg(&root)
        .args(["--authority", &root_hex, "--scope", "3.7"])
        .args(["--topology-epoch", "0", "--revision", "1"])
        .args(["--subject", &device, "--minimum-generation", "2"])
        .args(["--verifier", "self", "--state-dir"])
        .arg(operator.state())
        .output()
        .unwrap();
    assert!(removed.status.success(), "{removed:?}");
    wait_past(third);
    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["renewed"], false, "{}", node.ready);
    assert!(
        jsub["renew_error"].as_str().unwrap().contains("revoked"),
        "{}",
        node.ready
    );
    assert_eq!(jsub["admitted"], false, "{}", node.ready);
}

/// Sleep until the wall clock is past `at` (Unix seconds).
fn wait_past(at: u64) {
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now > at {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A loopback port free for both UDP (the mesh) and TCP (enrollment, which
/// shares an explicit bind port), so a restarted operator keeps the address
/// its bundle contact names.
fn free_mesh_port() -> u16 {
    loop {
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// Poll the device's `node status` until its live link satisfies `done`.
fn wait_for_link(fx: &Fx, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..180 {
        last = fx.json(&["node", "status"]);
        if done(&last["link"]) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("{what}: link never got there; last status {last}");
}

/// A joined node keeps itself attached: after the operator restarts, it
/// reattaches and presents its subnet credentials again (admission is per
/// session and the restarted verifier holds none); and a node that starts
/// while the operator is down comes up unattached and attaches once the
/// operator is back — nobody touches the device in either case.
#[test]
fn a_joined_node_reattaches_and_is_readmitted_without_being_touched() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (_root, _root_hex, issuer, grant) = ceremony(&keys);
    let bind = format!("127.0.0.1:{}", free_mesh_port());
    let op_args = [
        "--enroll",
        "--no-port-mapping",
        "--subnet-issuer-grant",
        grant.to_str().unwrap(),
        "--subnet-issuer-key",
        issuer.to_str().unwrap(),
    ];

    let op = operator.up_on(&bind, &op_args);
    let created = operator.json(&["invite", "create", "--subnet", "3.7"]);
    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["subnet"]["credentials"], "installed", "{joined}");
    let node = agent.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["admitted"], true,
        "{}",
        node.ready
    );

    // A: the operator restarts under the running device — with the same
    // Noise key its bundle pinned, or the device could never reach it.
    let pinned = op.ready["public_key"].clone();
    drop(op);
    let op = operator.up_on(&bind, &op_args);
    assert_eq!(
        op.ready["public_key"], pinned,
        "the Noise key survives restart"
    );
    let status = wait_for_link(&agent, "reattach after operator restart", |l| {
        l["readmissions"].as_u64() >= Some(1)
            && l["attached"] == true
            && l["subnet_admitted"] == true
    });
    assert_eq!(status["link"]["path"], "direct", "{status}");
    drop(node);

    // B: the device starts while the operator is down.
    drop(op);
    let node = agent.up(&[]);
    assert_eq!(node.ready["joined"]["attached"], false, "{}", node.ready);
    let _op = operator.up_on(&bind, &op_args);
    wait_for_link(&agent, "attach once the operator is back", |l| {
        l["attached"] == true && l["subnet_admitted"] == true
    });
    drop(node);
}
