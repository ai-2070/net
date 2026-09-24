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

/// V3-2 task 3, end to end: a device already joined (with subnet 3.7) joins
/// a second subnet, 3.8, with a standalone link — over its own session, no
/// new token ceremony. It is admitted by the verifier; the membership
/// survives a restart (re-presented by the node itself); the link binds the
/// device that redeemed it (a second device is refused); a mesh invite, or a
/// link from a node this device did not enroll with, is refused; and an
/// approval-gated link completes by itself once the operator approves.
#[test]
fn a_joined_device_joins_another_subnet_with_a_standalone_link() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (_root, _root_hex, issuer, grant) = ceremony(&keys);
    let op_args = [
        "--enroll",
        "--no-port-mapping",
        "--subnet-issuer-grant",
        grant.to_str().unwrap(),
        "--subnet-issuer-key",
        issuer.to_str().unwrap(),
    ];
    let _op = operator.up(&op_args);

    let created = operator.json(&["invite", "create", "--subnet", "3.7"]);
    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    let device = joined["device"].as_str().unwrap().to_string();
    let node = agent.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["admitted"], true,
        "{}",
        node.ready
    );

    // The operator makes a standalone link for 3.8.
    let link = operator.json(&["subnet", "invite", "3.8"]);
    assert_eq!(link["standalone"], true, "{link}");
    assert_eq!(link["subnet"]["scope"], "3.8", "{link}");
    let link_token = token_of(&link);

    // A mesh invite is not a standalone link.
    let mesh_invite = operator.json(&["invite", "create"]);
    let refused = agent.run(&["subnet", "join", &token_of(&mesh_invite), "--yes"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a standalone subnet link"),
        "{refused:?}"
    );

    // One active attachment per verifier: 3.7 is active, so joining 3.8 at
    // the same verifier is a switch, and a switch is explicit. The refusal
    // precedes redemption, so the link is not consumed.
    let refused = agent.run(&["subnet", "join", &link_token, "--yes"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--switch"),
        "{refused:?}"
    );
    let sub = agent.json(&["subnet", "join", &link_token, "--yes", "--switch"]);
    assert_eq!(sub["state"], "installed", "{sub}");
    assert_eq!(sub["scope"], "3.8", "{sub}");
    assert_eq!(sub["active"], true, "{sub}");
    assert_eq!(sub["previous"], "3.7", "{sub}");
    assert_eq!(sub["previous_withdrawal"], "confirmed", "{sub}");
    assert_eq!(sub["admitted"], true, "{sub}");
    assert_eq!(sub["device"], device.as_str(), "the same proven identity");

    // The verifier's own view: the device is attached at exactly one scope.
    let attached_at = |fx: &Fx| -> Vec<String> {
        fx.json(&["subnet", "members", "3"])["observed"]["admitted_here"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["subject"] == device.as_str())
            .map(|r| r["attachment"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(attached_at(&operator), vec!["3.8".to_string()]);

    // Restart: the node presents only the active attachment, deterministically
    // — the stored 3.7 is reported stored, and no supervisor flips them.
    drop(node);
    let node = agent.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["active"], false,
        "{}",
        node.ready
    );
    wait_for_link(&agent, "only the active attachment re-presented", |l| {
        l["attached"] == true
            && l["subnet_active"] == false
            && l.get("subnet_admitted").is_none()
            && l["standalone"].as_object().is_some_and(|m| {
                m.values()
                    .any(|e| e["scope"] == "3.8" && e["active"] == true && e["admitted"] == true)
            })
    });
    for _ in 0..3 {
        assert_eq!(
            attached_at(&operator),
            vec!["3.8".to_string()],
            "no flipping"
        );
        std::thread::sleep(Duration::from_millis(1500));
    }

    // The explicit switch back.
    let switched = agent.json(&["subnet", "activate", "3.7"]);
    assert_eq!(switched["changed"], true, "{switched}");
    assert_eq!(switched["previous"], "3.8", "{switched}");
    assert_eq!(switched["previous_withdrawal"], "confirmed", "{switched}");
    assert_eq!(switched["admitted"], true, "{switched}");
    assert_eq!(attached_at(&operator), vec!["3.7".to_string()]);
    assert_eq!(
        agent.json(&["subnet", "activate", "3.7"])["changed"],
        false,
        "activating the active attachment is a no-op"
    );

    // The link binds the device that redeemed it: another joined device,
    // over its own proven session, gets nothing.
    let second = Fx::new();
    let other_invite = operator.json(&["invite", "create", "--subnet", "3.7"]);
    second.json(&["join", &token_of(&other_invite), "--yes"]);
    let other_node = second.up(&[]);
    let stolen = second.run(&["subnet", "join", &link_token, "--yes", "--switch"]);
    assert!(!stolen.status.success(), "{stolen:?}");
    assert!(
        String::from_utf8_lossy(&stolen.stderr).contains("bound to another claim"),
        "{stolen:?}"
    );
    drop(other_node);

    // A link from a node this device did not enroll with is refused.
    let elsewhere = Fx::new();
    let _other_op = elsewhere.up(&op_args);
    let foreign = elsewhere.json(&["subnet", "invite", "3.8"]);
    let refused = agent.run(&["subnet", "join", &token_of(&foreign), "--yes"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("not by the node this device enrolled with"),
        "{refused:?}"
    );

    // Approval-gated: pending until approved; the node completes it by
    // itself and, 3.7 being active, keeps it STORED (a completion never
    // switches).
    let gated = operator.json(&["subnet", "invite", "3.9", "--require-approval"]);
    let pending = agent.json(&["subnet", "join", &token_of(&gated), "--yes", "--switch"]);
    assert_eq!(pending["state"], "pending_approval", "{pending}");
    let offer_id = gated["offer_id"].as_str().unwrap().to_string();
    operator.json(&["invite", "approve", &offer_id, "--subject", &device]);
    wait_for_link(&agent, "approved membership completes, stored", |l| {
        l["standalone"].as_object().is_some_and(|m| {
            m.values()
                .any(|e| e["scope"] == "3.9" && e["state"] == "installed" && e["active"] == false)
        })
    });
    assert_eq!(attached_at(&operator), vec!["3.7".to_string()]);
    // Leaving a stored relation withdraws nothing: 3.7 stays attached.
    let left = agent.json(&["subnet", "leave", "3.9"]);
    assert_eq!(left["was_active"], false, "{left}");
    assert_eq!(left["withdrawal"], "not_active", "{left}");
    assert_eq!(attached_at(&operator), vec!["3.7".to_string()]);
    // Leave covers the standalone memberships: each store records the
    // departure and holds no credentials any more.
    let left = agent.json(&["leave"]);
    assert_eq!(left["state"], "left", "{left}");
    drop(node);
    let subnets = agent.state().join("subnets");
    let mut seen = 0;
    for entry in std::fs::read_dir(&subnets).unwrap() {
        let dir = entry.unwrap().path();
        let membership = net_sdk::enrollment::standalone::SubnetMembership::open(&dir).unwrap();
        assert!(membership.left_at().is_some(), "{}", dir.display());
        assert!(membership.credentials().is_none(), "{}", dir.display());
        seen += 1;
    }
    assert_eq!(seen, 2, "the 3.8 and 3.9 memberships");
}

/// V3-3 (subnet): `subnet members` separates what the operator's node
/// ISSUED for a scope from which peers are admitted to it at that node RIGHT
/// NOW. An issued device that is not connected is issued-but-absent (not
/// removed); a device of another scope is in neither; a node that does not
/// enroll says its issuer inventory is unknown.
#[test]
fn subnet_members_separates_issued_from_admitted_here() {
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
    ]);

    // One device in 3.7 (connected), one in 3.8.
    let in_scope = Fx::new();
    let joined = in_scope.json(&[
        "join",
        &token_of(&operator.json(&["invite", "create", "--subnet", "3.7"])),
        "--yes",
    ]);
    let device = joined["device"].as_str().unwrap().to_string();
    let node = in_scope.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["admitted"], true,
        "{}",
        node.ready
    );
    let elsewhere = Fx::new();
    let other = elsewhere.json(&[
        "join",
        &token_of(&operator.json(&["invite", "create", "--subnet", "3.8"])),
        "--yes",
    ]);
    let other_device = other["device"].as_str().unwrap().to_string();

    let members = operator.json(&["subnet", "members", "3.7"]);
    let issued = members["issued"].as_array().unwrap();
    assert_eq!(issued.len(), 1, "{members}");
    assert_eq!(issued[0]["subject"], device.as_str(), "{members}");
    assert_eq!(issued[0]["state"], "issued", "{members}");
    assert_eq!(issued[0]["subnet"]["scope"], "3.7", "{members}");
    assert_eq!(members["unrecorded_offers"], 0, "{members}");
    let here = members["observed"]["admitted_here"].as_array().unwrap();
    assert_eq!(here.len(), 1, "{members}");
    assert_eq!(here[0]["subject"], device.as_str());
    assert_eq!(here[0]["attachment"], "3.7");

    // Signed observations from named nodes, asked with the authority root:
    // the operator's node (a verifier) observes the device; the device's own
    // node verifies for no subnet authority.
    let device_contact = format!(
        "{}@{}#{}",
        node.ready["entity_id"].as_str().unwrap(),
        node.ready["bind"].as_str().unwrap(),
        node.ready["public_key"].as_str().unwrap()
    );
    let remote = |key: &std::path::Path| {
        operator.json(&[
            "subnet",
            "members",
            "3.7",
            "--verifier",
            "self",
            "--verifier",
            &device_contact,
            "--root-key",
            key.to_str().unwrap(),
            "--authority",
            &root_hex,
        ])
    };
    let asked = remote(&root);
    let rows = asked["remote"].as_array().unwrap();
    assert_eq!(rows[0]["state"], "observed", "{asked}");
    assert_eq!(
        rows[0]["admitted"][0]["subject"],
        device.as_str(),
        "{asked}"
    );
    assert_eq!(rows[0]["admitted"][0]["attachment"], "3.7", "{asked}");
    assert_eq!(rows[1]["state"], "not_verifier", "{asked}");
    // Another scope, same verifier: observed, and nothing admitted there.
    let eight = operator.json(&[
        "subnet",
        "members",
        "3.8",
        "--verifier",
        "self",
        "--root-key",
        root.to_str().unwrap(),
        "--authority",
        &root_hex,
    ]);
    assert_eq!(eight["remote"][0]["state"], "observed", "{eight}");
    assert!(
        eight["remote"][0]["admitted"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{eight}"
    );
    // A key that is not the authority's root cannot read it.
    let denied = remote(&issuer);
    assert_eq!(denied["remote"][0]["state"], "refused", "{denied}");
    assert!(members["completeness"]["observed"]
        .as_str()
        .unwrap()
        .contains("other verifiers were not asked"));
    // The subtree query includes both; 3.8's device is issued, not connected.
    let all = operator.json(&["subnet", "members", "3"]);
    let subjects: Vec<&str> = all["issued"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["subject"].as_str())
        .collect();
    assert!(subjects.contains(&device.as_str()) && subjects.contains(&other_device.as_str()));
    assert_eq!(
        all["observed"]["admitted_here"].as_array().unwrap().len(),
        1
    );
    // The 3.8 view: its device is issued there, but nothing is admitted there.
    let eight = operator.json(&["subnet", "members", "3.8"]);
    assert_eq!(
        eight["issued"][0]["subject"],
        other_device.as_str(),
        "{eight}"
    );
    assert!(
        eight["observed"]["admitted_here"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{eight}"
    );

    // Stopped: still issued, no longer admitted here once its session is
    // silent — absent, not removed.
    drop(node);
    let mut last = Value::Null;
    let mut gone = false;
    for _ in 0..120 {
        last = operator.json(&["subnet", "members", "3.7"]);
        if last["observed"]["admitted_here"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        gone,
        "a stopped device must drop out of admitted_here: {last}"
    );
    assert_eq!(last["issued"][0]["state"], "issued", "{last}");

    // A node that does not enroll: its issuer inventory is unknown.
    let node = elsewhere.up(&[]);
    let theirs = elsewhere.json(&["subnet", "members", "3.8"]);
    assert!(theirs["issued"].is_null(), "{theirs}");
    assert!(theirs["completeness"]["issued"]
        .as_str()
        .unwrap()
        .starts_with("unknown"));
    drop(node);
}

/// V3-2B subnet leave: a device leaves its join's subnet relation through
/// its running `up`. The verifier acknowledges dropping exactly that
/// admission (its own observation shows the device gone). The relation is
/// never presented or renewed again (a short leaf passes its renewal point
/// untouched), it stays left across restart while the mesh attachment
/// remains, and repeating is idempotent.
#[test]
fn a_device_leaves_its_subnet_relation_and_is_withdrawn_at_the_verifier() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (_root, _root_hex, issuer, grant) = ceremony(&keys);
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
    agent.json(&["join", &token_of(&created), "--yes"]);
    let node = agent.up(&[]);
    let jsub = &node.ready["joined"]["subnet"];
    assert_eq!(jsub["admitted"], true, "{}", node.ready);
    let first = jsub["expires_at"].as_u64().unwrap();
    let admitted_here =
        |fx: &Fx| fx.json(&["subnet", "members", "3.7"])["observed"]["admitted_here"].clone();
    assert_eq!(admitted_here(&operator).as_array().unwrap().len(), 1);

    let left = agent.json(&["subnet", "leave", "3.7"]);
    assert_eq!(left["newly_left"], true, "{left}");
    assert_eq!(left["relation"], "join", "{left}");
    assert_eq!(left["withdrawal"], "confirmed", "{left}");
    assert_eq!(left["dropped"], true, "{left}");
    assert!(
        admitted_here(&operator).as_array().unwrap().is_empty(),
        "the verifier no longer admits the device"
    );
    assert_eq!(agent.json(&["subnet", "leave", "3.7"])["newly_left"], false);
    assert!(!agent.run(&["subnet", "leave", "3.9"]).status.success());

    // Past the renewal point (a third of the life before expiry): nothing
    // was renewed or presented again.
    wait_past(first.saturating_sub(3));
    let status = agent.json(&["node", "status"]);
    assert_eq!(status["subnet_expires_at"], first, "{status}");
    assert!(status["link"].get("subnet_admitted").is_none(), "{status}");
    assert!(admitted_here(&operator).as_array().unwrap().is_empty());

    // Restart: still left, still attached to the mesh.
    drop(node);
    let node = agent.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["state"], "left",
        "{}",
        node.ready
    );
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    std::thread::sleep(Duration::from_secs(2));
    assert!(admitted_here(&operator).as_array().unwrap().is_empty());
    drop(node);
}

/// E18/E19 (subnet): with no node running, `subnet leave` completes locally —
/// recorded durably, the active record released — while the verifier-side
/// cleanup is reported unconfirmed, not claimed. It is idempotent, and the
/// next `up` neither presents nor renews the relation.
#[test]
fn an_offline_subnet_leave_completes_locally_and_restart_honours_it() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (_root, _root_hex, issuer, grant) = ceremony(&keys);
    let _op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--subnet-issuer-grant",
        grant.to_str().unwrap(),
        "--subnet-issuer-key",
        issuer.to_str().unwrap(),
    ]);
    let created = operator.json(&["invite", "create", "--subnet", "3.7"]);
    let agent = Fx::new();
    agent.json(&["join", &token_of(&created), "--yes"]);
    let node = agent.up(&[]);
    assert_eq!(node.ready["joined"]["subnet"]["admitted"], true);
    drop(node);

    let left = agent.json(&["subnet", "leave", "3.7"]);
    assert_eq!(left["runtime"], "not_running", "{left}");
    assert_eq!(left["newly_left"], true, "{left}");
    assert_eq!(left["was_active"], true, "{left}");
    assert!(
        left["withdrawal"]
            .as_str()
            .unwrap()
            .starts_with("unconfirmed"),
        "{left}"
    );
    assert_eq!(agent.json(&["subnet", "leave", "3.7"])["newly_left"], false);

    let node = agent.up(&[]);
    assert_eq!(
        node.ready["joined"]["subnet"]["state"], "left",
        "{}",
        node.ready
    );
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    std::thread::sleep(Duration::from_secs(2));
    let members = operator.json(&["subnet", "members", "3.7"]);
    assert!(
        members["observed"]["admitted_here"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{members}"
    );
    drop(node);
}
