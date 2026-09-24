//! Channel links through the CLI (NET_CLI_PLAN_V3 V3-2A C2): the channel
//! root is an operator identity that stays offline and signs one grant to
//! the operator node (`channel issue-grant`); the node (`up --enroll
//! --channel-grant`) mints each joining device a chain `root → node →
//! device`; a subscribe link is created only while this node serves the
//! channel trusting that root (`channel serve`, persisted across restarts);
//! and a corrupt served-channel record fails the start closed. Real
//! subprocesses on loopback with router mapping disabled.
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

const CHANNEL: &str = "fleet.telemetry";

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

    /// One-shot command with `--output json`, plus `--state-dir` when
    /// `stateful`; killed and reported if still running after 20 s.
    fn exec(&self, args: &[&str], stateful: bool) -> Output {
        let mut cmd = self.base();
        cmd.args(["--output", "json"]).args(args);
        if stateful {
            cmd.arg("--state-dir").arg(self.state());
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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
        self.exec(args, true)
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

    fn json_stateless(&self, args: &[&str]) -> Value {
        let out = self.exec(args, false);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn spawn_up(&self, extra: &[&str]) -> Child {
        self.base()
            .args(["--output", "ndjson", "up", "--bind", "127.0.0.1:0"])
            .args(extra)
            .arg("--state-dir")
            .arg(self.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn up(&self, extra: &[&str]) -> Up {
        let mut child = self.spawn_up(extra);
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

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The offline channel root: an operator identity file and its entity hex.
fn channel_root(fx: &Fx, dir: &Path) -> (PathBuf, String) {
    let path = dir.join("channel-root.toml");
    let made = fx.json_stateless(&["identity", "generate", "--out", path.to_str().unwrap()]);
    (path, made["public_key_hex"].as_str().unwrap().to_string())
}

#[test]
fn a_device_joins_with_a_channel_credential_minted_from_an_offline_grant() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (root, root_hex) = channel_root(&operator, &keys);

    // The node's identity is what the root delegates to: learn it from a
    // first start, then sign the grant offline.
    let first = operator.up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let node_hex = first.ready["enrollment"]["issuer"]
        .as_str()
        .unwrap()
        .to_string();
    drop(first);
    let grant = keys.join("fleet.grant");
    let issued = operator.json_stateless(&[
        "channel",
        "issue-grant",
        "--root-identity",
        root.to_str().unwrap(),
        "--issuer",
        &node_hex,
        "--channel",
        CHANNEL,
        "--out",
        grant.to_str().unwrap(),
    ]);
    assert_eq!(issued["root"], root_hex.as_str(), "{issued}");
    assert_eq!(issued["rights"], "publish,subscribe");
    // The grant file holds no secret: the root's seed never leaves its file.
    let grant_text = std::fs::read_to_string(&grant).unwrap();
    let root_text = std::fs::read_to_string(&root).unwrap();
    let seed = root_text
        .lines()
        .find_map(|l| l.trim().strip_prefix("seed_hex"))
        .unwrap()
        .trim_start_matches(['=', ' '])
        .trim()
        .trim_matches('"');
    assert!(!grant_text.contains(seed));
    // Refuses to overwrite without --force.
    let again = operator.exec(
        &[
            "channel",
            "issue-grant",
            "--root-identity",
            root.to_str().unwrap(),
            "--issuer",
            &node_hex,
            "--channel",
            CHANNEL,
            "--out",
            grant.to_str().unwrap(),
        ],
        false,
    );
    assert!(!again.status.success());

    let op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--channel-grant",
        grant.to_str().unwrap(),
    ]);
    let channels = &op.ready["enrollment"]["channels"];
    assert_eq!(channels[0]["channel"], CHANNEL, "{}", op.ready);
    assert_eq!(channels[0]["root"], root_hex.as_str());

    // Subscribe sends the device to this node: refused until it serves the
    // channel trusting the root.
    let refused = operator.run(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "subscribe",
    ]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        stderr_of(&refused).contains("channel serve"),
        "{}",
        stderr_of(&refused)
    );
    // No grant for another channel; rights are named explicitly.
    let refused = operator.run(&[
        "invite",
        "create",
        "--channel",
        "fleet.other",
        "--channel-rights",
        "publish",
    ]);
    assert!(stderr_of(&refused).contains("no grant for channel"));
    assert!(!operator
        .run(&["invite", "create", "--channel", CHANNEL])
        .status
        .success());
    let refused = operator.run(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "admin",
    ]);
    assert!(!refused.status.success());

    // Serving under another root does not satisfy the check.
    let stranger = "11".repeat(32);
    operator.json(&["channel", "serve", CHANNEL, "--token-root", &stranger]);
    assert!(!operator
        .run(&[
            "invite",
            "create",
            "--channel",
            CHANNEL,
            "--channel-rights",
            "subscribe"
        ])
        .status
        .success());
    let served = operator.json(&["channel", "serve", CHANNEL, "--token-root", &root_hex]);
    assert_eq!(served["served"], true, "{served}");
    assert_eq!(served["token_roots"][0], root_hex.as_str());

    let created = operator.json(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "subscribe",
    ]);
    assert_eq!(created["channel"]["channel"], CHANNEL, "{created}");
    assert_eq!(created["channel"]["rights"], "subscribe");
    assert_eq!(created["channel"]["root"], root_hex.as_str());
    let token = created["token"].as_str().unwrap().to_string();
    let inspected = operator.json_stateless(&["invite", "inspect", &token]);
    assert_eq!(inspected["channel"]["channel"], CHANNEL, "{inspected}");
    assert_eq!(
        inspected["relations"],
        serde_json::json!(["mesh", "channel"])
    );

    // The device joins: its bundle verified the chain against the signed
    // offer (a missing or foreign chain fails the join).
    let agent = Fx::new();
    let joined = agent.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(joined["channel"]["credential"], "stored", "{joined}");
    assert_eq!(joined["channel"]["rights"], "subscribe");

    // A publish link needs no serving here (the device's runtime must trust
    // the root itself).
    let publish = operator.json(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "publish",
    ]);
    assert_eq!(publish["channel"]["rights"], "publish", "{publish}");
    drop(op);

    // Serving is persisted: after a restart the subscribe link is still
    // creatable without serving again.
    let op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--channel-grant",
        grant.to_str().unwrap(),
    ]);
    operator.json(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "subscribe",
    ]);
    drop(op);

    // A corrupt served-channel record fails the start closed.
    std::fs::write(operator.state().join("channels.json"), b"{not json").unwrap();
    let mut child = operator.spawn_up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let mut exited = None;
    for _ in 0..400 {
        if let Some(status) = child.try_wait().unwrap() {
            exited = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = exited.unwrap_or_else(|| {
        let _ = child.kill();
        panic!("up started over a corrupt channels.json")
    });
    assert!(!status.success());
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(err.contains("channels.json"), "{err}");
}

/// A grant must name this node, and a grant file that does not match the
/// channel it names is refused at start.
#[test]
fn a_grant_for_another_node_is_refused_at_start() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (root, _) = channel_root(&operator, &keys);
    let grant = keys.join("elsewhere.grant");
    operator.json_stateless(&[
        "channel",
        "issue-grant",
        "--root-identity",
        root.to_str().unwrap(),
        "--issuer",
        &"22".repeat(32),
        "--channel",
        CHANNEL,
        "--out",
        grant.to_str().unwrap(),
    ]);
    let mut child = operator.spawn_up(&[
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--channel-grant",
        grant.to_str().unwrap(),
    ]);
    let status = child.wait().unwrap();
    assert!(!status.success());
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(err.contains("another identity"), "{err}");

    // A grant file whose channel name was edited no longer matches its
    // signed hash.
    let mut file: Value = serde_json::from_slice(&std::fs::read(&grant).unwrap()).unwrap();
    file["channel"] = Value::from("fleet.other");
    std::fs::write(&grant, serde_json::to_vec(&file).unwrap()).unwrap();
    let mut child = operator.spawn_up(&[
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--channel-grant",
        grant.to_str().unwrap(),
    ]);
    assert!(!child.wait().unwrap().success());
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(err.contains("not for the channel it names"), "{err}");
}

fn free_mesh_port() -> u16 {
    loop {
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

impl Fx {
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

/// The operator's offline ceremony and start arguments: a channel root, the
/// grant to this operator's enrollment issuer, and `up` flags carrying it.
fn operator_with_grant(operator: &Fx) -> (Vec<String>, String) {
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (root, root_hex) = channel_root(operator, &keys);
    let first = operator.up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let node_hex = first.ready["enrollment"]["issuer"]
        .as_str()
        .unwrap()
        .to_string();
    drop(first);
    let grant = keys.join("fleet.grant");
    operator.json_stateless(&[
        "channel",
        "issue-grant",
        "--root-identity",
        root.to_str().unwrap(),
        "--issuer",
        &node_hex,
        "--channel",
        CHANNEL,
        "--out",
        grant.to_str().unwrap(),
    ]);
    let args = [
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--channel-grant",
        grant.to_str().unwrap(),
    ]
    .map(str::to_string)
    .to_vec();
    (args, root_hex)
}

fn strs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Poll the device's `channel status` until its credential state satisfies
/// `done`.
fn wait_for_channel(fx: &Fx, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..180 {
        last = fx.json(&["channel", "status"]);
        if done(&last["joined"]) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("{what}: never got there; last status {last}");
}

/// Subscribe is the publisher's ACK of the FULL chain on each session: the
/// device is ACKed at start, re-subscribes by itself after the operator
/// restarts, and `channel leave` records the departure, is acknowledged by
/// the publisher, and survives the device's restart. A plain `leave` of a
/// subscribed device withdraws the subscription before it stops.
#[test]
fn a_subscribing_device_is_acked_resubscribes_and_leaves() {
    let operator = Fx::new();
    let (op_args, root_hex) = operator_with_grant(&operator);
    let bind = format!("127.0.0.1:{}", free_mesh_port());
    let op = operator.up_on(&bind, &strs(&op_args));
    operator.json(&["channel", "serve", CHANNEL, "--token-root", &root_hex]);
    let invite = |fx: &Fx| {
        let created = fx.json(&[
            "invite",
            "create",
            "--channel",
            CHANNEL,
            "--channel-rights",
            "subscribe",
        ]);
        created["token"].as_str().unwrap().to_string()
    };

    let agent = Fx::new();
    agent.json(&["join", &invite(&operator), "--yes"]);
    let node = agent.up(&[]);
    let ch = &node.ready["joined"]["channel"];
    assert_eq!(ch["state"], "active", "{}", node.ready);
    assert_eq!(ch["subscribed"], true, "{}", node.ready);
    assert_eq!(ch["rights"], "subscribe");
    assert_eq!(ch["root"], root_hex.as_str());
    assert!(ch.get("publish_installed").is_none(), "subscribe only");

    // The operator restarts (its served channel is re-applied from state):
    // the device re-attaches and presents its chain again, untouched.
    drop(op);
    let op = operator.up_on(&bind, &strs(&op_args));
    wait_for_channel(&agent, "resubscribe after operator restart", |j| {
        j["resubscribes"].as_u64() >= Some(1) && j["subscribed"] == true
    });

    // Channel-only leave: recorded, acknowledged, idempotent, durable.
    let left = agent.json(&["channel", "leave"]);
    assert_eq!(left["newly_left"], true, "{left}");
    assert_eq!(left["unsubscribed"], true, "{left}");
    let status = agent.json(&["channel", "status"]);
    assert_eq!(status["joined"]["state"], "left", "{status}");
    assert!(status["joined"].get("subscribed").is_none(), "{status}");
    assert_eq!(agent.json(&["channel", "leave"])["newly_left"], false);
    drop(node);
    let node = agent.up(&[]);
    let ch = &node.ready["joined"]["channel"];
    assert_eq!(ch["state"], "left", "{}", node.ready);
    assert!(ch.get("subscribed").is_none(), "{}", node.ready);
    // The mesh membership is untouched by a channel leave.
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    drop(node);

    // A whole-mesh leave of a subscribed device withdraws the subscription.
    let second = Fx::new();
    second.json(&["join", &invite(&operator), "--yes"]);
    let running = second.up(&[]);
    assert_eq!(running.ready["joined"]["channel"]["subscribed"], true);
    let gone = second.json(&["leave"]);
    assert_eq!(gone["state"], "left", "{gone}");
    assert_eq!(gone["channel_unsubscribed"], true, "{gone}");
    drop(running);
    drop(op);
}

/// Publish is local: the chain is installed as the device's managed publish
/// credential, ready only once the device's OWN config trusts the root (no
/// root is installed implicitly), and `channel leave` removes exactly that
/// credential with the stop confirmed.
#[test]
fn a_publishing_device_is_ready_only_under_its_own_trust_and_leaves_exactly() {
    let operator = Fx::new();
    let (op_args, root_hex) = operator_with_grant(&operator);
    let _op = operator.up(&strs(&op_args));
    let created = operator.json(&[
        "invite",
        "create",
        "--channel",
        CHANNEL,
        "--channel-rights",
        "publish",
    ]);
    let agent = Fx::new();
    agent.json(&["join", created["token"].as_str().unwrap(), "--yes"]);
    let node = agent.up(&[]);
    let ch = &node.ready["joined"]["channel"];
    assert_eq!(ch["publish_installed"], true, "{}", node.ready);
    assert_eq!(ch["publish_ready"], false, "no local trust yet");
    assert!(ch.get("subscribed").is_none(), "publish only");
    let publish = |fx: &Fx| fx.run(&["channel", "publish", CHANNEL, "--data", "reading 42"]);
    // Ungated here: the publish is reported open, not credential evidence.
    let open: Value = serde_json::from_slice(&publish(&agent).stdout).unwrap();
    assert!(open["gate"].as_str().unwrap().starts_with("open"), "{open}");
    assert!(agent.json(&["channel", "status"])["joined"]
        .get("published_at")
        .is_none());

    // Trusting some other root for the channel is not trusting this one.
    agent.json(&[
        "channel",
        "serve",
        CHANNEL,
        "--token-root",
        &"33".repeat(32),
    ]);
    std::thread::sleep(Duration::from_millis(2500));
    let status = agent.json(&["channel", "status"]);
    assert_eq!(status["joined"]["publish_ready"], false, "{status}");
    // A real denial at the local production gate.
    let denied = publish(&agent);
    assert!(!denied.status.success());
    assert!(
        stderr_of(&denied).contains("publish denied by channel ACL"),
        "{}",
        stderr_of(&denied)
    );
    agent.json(&["channel", "serve", CHANNEL, "--token-root", &root_hex]);
    wait_for_channel(&agent, "publish ready under local trust", |j| {
        j["publish_ready"] == true
    });
    // A caller-requested publish clears the gate with the managed chain:
    // live-active, distinct from ready.
    let passed = agent.json(&["channel", "publish", CHANNEL, "--data", "reading 42"]);
    assert_eq!(passed["gate"], "passed", "{passed}");
    let status = agent.json(&["channel", "status"]);
    assert!(
        status["joined"]["published_at"].as_u64().is_some(),
        "{status}"
    );

    let left = agent.json(&["channel", "leave"]);
    assert_eq!(left["publish_removed"], true, "{left}");
    assert_eq!(left["publish_stop"], "confirmed", "{left}");
    let status = agent.json(&["channel", "status"]);
    assert_eq!(status["joined"]["publish_installed"], false, "{status}");
    assert_eq!(status["joined"]["publish_ready"], false, "{status}");
    // After leave the gate denies again: no fallback credential remains.
    assert!(stderr_of(&publish(&agent)).contains("publish denied by channel ACL"));
    // Serving the channel is independent of holding the credential.
    assert_eq!(status["served"][0]["channel"], CHANNEL, "{status}");
    drop(node);
}
