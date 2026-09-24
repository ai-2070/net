// SPDX-License-Identifier: MIT OR Apache-2.0
//! The V3 public journey (NET_CLI_PLAN_V3 V3-5 W2, E16): three participants
//! on one runner — an operator and two devices, B and C — every one a real
//! `net-mesh` subprocess on loopback with router mapping disabled. Loopback
//! proves multi-node/multi-process behaviour, not separate computers.
//!
//! 1. The operator runs `up --enroll` holding only delegated authority: a
//!    subnet issuer grant and a channel grant (both roots stay offline), and
//!    serves the channel trusting its root.
//! 2. B and C join with one composed link each (mesh + subnet + channel) and
//!    their `up` is admitted to the subnet and ACKed on the channel.
//! 3. B publishes through its own local channel gate (denied until it trusts
//!    the root, then passed).
//! 4. The operator removes B from the subnet: B's restart is refused
//!    admission with its old credentials while C's is admitted.
//! 5. B provides a tool and C invokes it, each as its ENROLLED device
//!    (`--joined`): the consent gate refuses before any effect, then one
//!    approved call leaves exactly one provider-side record.
//! 6. C leaves (the subscription is withdrawn), restart stays left, and the
//!    operator goes down cleanly.
#[path = "fixtures/capability_client.rs"]
mod client;

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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

    fn base(&self) -> std::process::Command {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"));
        c
    }

    fn async_base(&self) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        c
    }

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
        for _ in 0..600 {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        panic!(
            "{args:?} did not exit within 30s: {:?}",
            child.wait_with_output()
        );
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.exec(args, true);
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

    fn up(&self, extra: &[&str]) -> Up {
        let mut child = self
            .base()
            .args(["--output", "ndjson", "up", "--bind", "127.0.0.1:0"])
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

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A subnet key file (`subnet keygen`) and its entity hex.
fn keygen(fx: &Fx, dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let made = fx.json_stateless(&["subnet", "keygen", "--out", key.to_str().unwrap()]);
    (key, made["entity_id_hex"].as_str().unwrap().to_string())
}

fn origin_hash(entity_hex: &str) -> u64 {
    net::adapter::net::identity::EntityId::from_bytes(
        hex::decode(entity_hex).unwrap().try_into().unwrap(),
    )
    .origin_hash()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_participants_join_publish_are_selectively_removed_and_invoke_as_enrolled_devices() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();

    // ── Offline ceremonies: roots never reach the node ──────────────────
    let (subnet_root, subnet_hex) = keygen(&operator, &keys, "subnet-root.toml");
    let (subnet_issuer, subnet_issuer_hex) = keygen(&operator, &keys, "subnet-issuer.toml");
    let subnet_grant = keys.join("subnet-issuer.grant");
    operator.json_stateless(&[
        "subnet",
        "issue-issuer",
        "--root-key",
        subnet_root.to_str().unwrap(),
        "--authority",
        &subnet_hex,
        "--issuer",
        &subnet_issuer_hex,
        "--scope",
        "3",
        "--max-rights",
        "attach",
        "--out",
        subnet_grant.to_str().unwrap(),
    ]);
    let channel_root = keys.join("channel-root.toml");
    let channel_root_hex = operator.json_stateless(&[
        "identity",
        "generate",
        "--out",
        channel_root.to_str().unwrap(),
    ])["public_key_hex"]
        .as_str()
        .unwrap()
        .to_string();
    // The channel grant names the node's enrollment issuer: learn it first.
    let first = operator.up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let issuer_hex = first.ready["enrollment"]["issuer"]
        .as_str()
        .unwrap()
        .to_string();
    drop(first);
    let channel_grant = keys.join("channel.grant");
    operator.json_stateless(&[
        "channel",
        "issue-grant",
        "--root-identity",
        channel_root.to_str().unwrap(),
        "--issuer",
        &issuer_hex,
        "--channel",
        CHANNEL,
        "--out",
        channel_grant.to_str().unwrap(),
    ]);

    // ── 1. The operator node ─────────────────────────────────────────────
    let op = operator.up(&[
        "--enroll",
        "--no-port-mapping",
        "--no-relay",
        "--subnet-issuer-grant",
        subnet_grant.to_str().unwrap(),
        "--subnet-issuer-key",
        subnet_issuer.to_str().unwrap(),
        "--channel-grant",
        channel_grant.to_str().unwrap(),
    ]);
    assert_eq!(op.ready["enrollment"]["subnet"]["verifier"], true);
    operator.json(&[
        "channel",
        "serve",
        CHANNEL,
        "--token-root",
        &channel_root_hex,
    ]);

    // ── 2. B and C join with one composed link each ──────────────────────
    let link = |rights: &str| {
        let created = operator.json(&[
            "invite",
            "create",
            "--subnet",
            "3.7",
            "--channel",
            CHANNEL,
            "--channel-rights",
            rights,
        ]);
        assert_eq!(created["channel"]["rights"], rights, "{created}");
        created["token"].as_str().unwrap().to_string()
    };
    let b = Fx::new();
    let c = Fx::new();
    let b_joined = b.json(&["join", &link("publish,subscribe"), "--yes"]);
    let c_joined = c.json(&["join", &link("subscribe"), "--yes"]);
    for joined in [&b_joined, &c_joined] {
        assert_eq!(joined["state"], "joined", "{joined}");
        assert_eq!(joined["subnet"]["credentials"], "installed", "{joined}");
        assert_eq!(joined["channel"]["credential"], "stored", "{joined}");
    }
    let b_device = b_joined["device"].as_str().unwrap().to_string();
    let c_device = c_joined["device"].as_str().unwrap().to_string();
    let b_node = b.up(&[]);
    let c_node = c.up(&[]);
    for node in [&b_node, &c_node] {
        let j = &node.ready["joined"];
        assert_eq!(j["subnet"]["admitted"], true, "{}", node.ready);
        assert_eq!(j["channel"]["subscribed"], true, "{}", node.ready);
    }

    // ── 3. B publishes through its own local gate ───────────────────────
    let denied = b.exec(&["channel", "publish", CHANNEL, "--data", "t=21.5"], true);
    // Ungated at B until it serves the channel: reported open, not evidence.
    let open: Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert!(open["gate"].as_str().unwrap().starts_with("open"), "{open}");
    b.json(&[
        "channel",
        "serve",
        CHANNEL,
        "--token-root",
        &"44".repeat(32),
    ]);
    let refused = b.exec(&["channel", "publish", CHANNEL, "--data", "t=21.5"], true);
    assert!(stderr_of(&refused).contains("publish denied by channel ACL"));
    b.json(&[
        "channel",
        "serve",
        CHANNEL,
        "--token-root",
        &channel_root_hex,
    ]);
    let passed = b.json(&["channel", "publish", CHANNEL, "--data", "t=21.5"]);
    assert_eq!(passed["gate"], "passed", "{passed}");

    // ── 4. Selective removal: B out of 3.7, C untouched ─────────────────
    let removed = operator
        .base()
        .args(["--output", "json", "subnet", "remove", "--root-key"])
        .arg(&subnet_root)
        .args(["--authority", &subnet_hex, "--scope", "3.7"])
        .args(["--topology-epoch", "0", "--revision", "1"])
        .args(["--subject", &b_device, "--minimum-generation", "2"])
        .args(["--verifier", "self", "--state-dir"])
        .arg(operator.state())
        .output()
        .unwrap();
    assert!(removed.status.success(), "{removed:?}");
    let removed: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed["complete"], true, "{removed}");
    drop(b_node);
    drop(c_node);
    let b_node = b.up(&[]);
    let c_node = c.up(&[]);
    let b_sub = &b_node.ready["joined"]["subnet"];
    assert_eq!(b_sub["admitted"], false, "{}", b_node.ready);
    assert!(b_sub["detail"].as_str().unwrap().contains("revoked"));
    assert_eq!(
        c_node.ready["joined"]["subnet"]["admitted"], true,
        "{}",
        c_node.ready
    );
    let members = operator.json(&["subnet", "members", "3.7"]);
    let here: Vec<&str> = members["observed"]["admitted_here"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["subject"].as_str().unwrap())
        .collect();
    assert_eq!(here, vec![c_device.as_str()], "{members}");
    drop(b_node);
    drop(c_node);

    // ── 5. B provides, C invokes — each as its enrolled device ──────────
    let audit = b.tmp.path().join("invocations.jsonl");
    let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut provider = b
        .async_base()
        .args(["--output", "ndjson", "--timeout", "30s", "wrap", "journey"])
        .arg("--joined")
        .arg(b.state())
        .args(["--bind", "127.0.0.1:0"])
        .args(["--allow", &origin_hash(&c_device).to_string()])
        .arg("--")
        .arg(if cfg!(windows) { "python" } else { "python3" })
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/capability_server.py"
        ))
        .arg(&audit)
        .arg(control.local_addr().unwrap().to_string())
        .spawn()
        .unwrap();
    let (mut control, _) = tokio::time::timeout(Duration::from_secs(30), control.accept())
        .await
        .unwrap()
        .unwrap();
    let mut events = tokio::io::BufReader::new(provider.stdout.take().unwrap()).lines();
    let wrapped: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(30), events.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wrapped["event"], "wrapped", "{wrapped}");
    let provider_node = wrapped["connection"]["node_id"].as_str().unwrap();
    let provider_id = u64::from_str_radix(provider_node.trim_start_matches("0x"), 16).unwrap();

    let pins = c.tmp.path().join("pins.json");
    let mut caller = c
        .async_base()
        .args(["--timeout", "30s", "mcp", "serve", "--joined"])
        .arg(c.state())
        .args(["--bind", "127.0.0.1:0", "--pin-store"])
        .arg(&pins)
        // C attaches directly to B's provider node, as its enrolled self.
        .args([
            "--node-addr",
            wrapped["connection"]["bind"].as_str().unwrap(),
            "--node-pubkey",
            wrapped["connection"]["node_pubkey"].as_str().unwrap(),
            "--node-id",
            provider_node,
        ])
        .spawn()
        .unwrap();
    let mut mcp = client::Client::new(caller.stdin.take().unwrap(), caller.stdout.take().unwrap());
    mcp.initialize().await;
    let cap_id = format!("{provider_id}/journey_echo");
    let mut last = String::new();
    let discovered = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let found = mcp
                .tool("net_search_capabilities", json!({"query": "journey_echo"}))
                .await;
            last = client::text(&found).to_string();
            if let Ok(rows) = serde_json::from_str::<Value>(client::text(&found)) {
                if rows["capabilities"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r["cap_id"] == cap_id))
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        discovered.is_ok(),
        "C discovers B's tool across the mesh; last search: {}",
        last
    );
    let call = json!({"cap_id": cap_id, "arguments": {"message": "calibrate"}});
    // The consent gate refuses before any provider effect.
    let refused = mcp.tool("net_invoke_capability", call.clone()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(!audit.exists(), "consent refusal precedes the effect");
    let approved = c
        .base()
        .args(["mcp", "pin", "approve", &cap_id, "--pin-store"])
        .arg(&pins)
        .output()
        .unwrap();
    assert!(approved.status.success(), "{approved:?}");
    let result = mcp.tool("net_invoke_capability", call).await;
    assert_eq!(result["isError"], false, "{result}");
    let records = std::fs::read_to_string(&audit).unwrap();
    assert_eq!(
        records.lines().count(),
        1,
        "exactly one provider-side effect"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&records).unwrap(),
        json!({"message": "calibrate"})
    );
    control.write_all(b"stop").await.unwrap();
    drop(mcp);
    let _ = tokio::time::timeout(Duration::from_secs(15), caller.wait()).await;
    let _ = tokio::time::timeout(Duration::from_secs(15), provider.wait()).await;

    // ── 6. C leaves; restart stays left; the operator goes down ─────────
    let c_node = c.up(&[]);
    assert_eq!(c_node.ready["joined"]["channel"]["subscribed"], true);
    let left = c.json(&["leave"]);
    assert_eq!(left["state"], "left", "{left}");
    assert_eq!(left["channel_unsubscribed"], true, "{left}");
    drop(c_node);
    let refused = c
        .base()
        .args([
            "--output",
            "json",
            "up",
            "--bind",
            "127.0.0.1:0",
            "--state-dir",
        ])
        .arg(c.state())
        .output()
        .unwrap();
    assert!(!refused.status.success(), "a left device does not start");
    let down = operator.json(&["down"]);
    assert!(down.to_string().contains("stopped"), "{down}");
    drop(op);
}

/// `--joined` runs only as a complete, non-left join nobody else owns: a
/// running `up` on that state, a PSK flag, a partially named peer, a state
/// that never joined and a device that left are each refused before any
/// mesh effect.
#[test]
fn joined_consumers_refuse_what_they_cannot_honour() {
    let operator = Fx::new();
    let _op = operator.up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let created = operator.json(&["invite", "create"]);
    let device = Fx::new();
    device.json(&["join", created["token"].as_str().unwrap(), "--yes"]);
    let serve = |extra: &[&str]| {
        let mut cmd = device.base();
        cmd.args(["--timeout", "20s", "mcp", "serve", "--joined"])
            .arg(device.state())
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let out = cmd.output().unwrap();
        assert!(!out.status.success(), "{extra:?} must be refused");
        stderr_of(&out)
    };
    let running = device.up(&[]);
    assert!(serve(&[]).contains("in use"), "a running up owns the join");
    drop(running);
    assert!(serve(&["--psk-hex", &"42".repeat(32)]).contains("--psk-hex"));
    assert!(serve(&["--node-addr", "127.0.0.1:9"]).contains("--node-pubkey"));
    let stranger = Fx::new();
    let out = stranger
        .base()
        .args(["mcp", "serve", "--joined"])
        .arg(stranger.state())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        stderr_of(&out).contains("has not joined"),
        "{}",
        stderr_of(&out)
    );
    device.json(&["leave"]);
    assert!(serve(&[]).contains("left the mesh"));
}

/// E2: inspecting a link (and listing the ledger) leaves the enrollment
/// ledger byte-for-byte untouched and consumes nothing — the same link still
/// redeems afterwards, and that redemption does change the ledger (so the
/// comparison is sensitive).
#[test]
fn inspection_leaves_the_ledger_untouched_and_the_link_redeemable() {
    let operator = Fx::new();
    let _op = operator.up(&["--enroll", "--no-port-mapping", "--no-relay"]);
    let created = operator.json(&["invite", "create"]);
    let token = created["token"].as_str().unwrap().to_string();
    let ledger = operator.state().join("ledger").join("enrollment.snapshot");
    let before = std::fs::read(&ledger).unwrap();
    for _ in 0..2 {
        let inspected = operator.json_stateless(&["invite", "inspect", &token]);
        assert_eq!(inspected["relations"], json!(["mesh"]), "{inspected}");
        let status = operator.json(&["invite", "status"]);
        assert!(status.to_string().contains("offered"), "{status}");
    }
    assert_eq!(
        std::fs::read(&ledger).unwrap(),
        before,
        "inspection wrote nothing"
    );
    let device = Fx::new();
    let joined = device.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_ne!(
        std::fs::read(&ledger).unwrap(),
        before,
        "redemption is recorded"
    );
}
