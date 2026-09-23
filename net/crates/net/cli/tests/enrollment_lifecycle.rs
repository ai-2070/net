//! `net-mesh up --enroll`, `enrollment init` and `invite ...` as real
//! subprocesses, with a clean device redeeming the printed join token through
//! the SDK and attaching to the running node's mesh. Loopback, one host: the
//! device-side `join` command is a later slice.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError, JoinStatus};
use net_sdk::enrollment::invite::MembershipInvite;
use net_sdk::enrollment::redeem::{RedeemError, Refusal};
use net_sdk::identity::Identity;
use serde_json::Value;

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

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn state(&self) -> PathBuf {
        self.path("state")
    }

    fn base(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(self.path("config.toml"));
        c
    }

    /// Run with `--output json` and `--state-dir`, capturing everything. A
    /// one-shot command still running after 20 s is killed and reported, so a
    /// refusal that regresses into a live node fails fast instead of hanging.
    fn run(&self, args: &[&str]) -> Output {
        let mut child = self
            .base()
            .args(["--output", "json"])
            .args(args)
            .arg("--state-dir")
            .arg(self.state())
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
        let out = child.wait_with_output().unwrap();
        panic!("{args:?} did not exit within 20s (still running): {out:?}");
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

    fn issuer(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let out = self
            .base()
            .args(["--output", "json", "identity", "generate", "--out"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        path
    }

    fn init(&self, issuer: &Path) -> Output {
        self.run(&[
            "enrollment",
            "init",
            "--issuer-identity",
            issuer.to_str().unwrap(),
        ])
    }

    fn up_enroll(&self, issuer: &Path, port: u16) -> Up {
        let addr = format!("127.0.0.1:{port}");
        let mut child = self
            .base()
            .args([
                "--output",
                "ndjson",
                "up",
                "--enroll",
                "--no-port-mapping",
                "--bind",
                &addr,
            ])
            .args(["--public-addr", &addr, "--issuer-identity"])
            .arg(issuer)
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

/// A port currently free for both UDP and TCP on loopback (TCP first: see the
/// CLI's own port picker for why).
fn free_port() -> u16 {
    for _ in 0..50 {
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free port");
}

/// A clean device redeems `token` with the SDK join owner.
fn redeem(dir: &Path, token: &str) -> (Result<JoinStatus, DeviceJoinError>, DeviceJoin) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let invite = MembershipInvite::decode(token).unwrap();
    let mut join = DeviceJoin::begin(dir, &invite, Identity::generate()).unwrap();
    let status = rt.block_on(join.redeem(Duration::from_secs(10)));
    (status, join)
}

/// Attach to the node named by an installed bundle, with its delivered PSK.
fn attach(join: &DeviceJoin) -> Result<(), String> {
    let bundle = join.bundle().expect("installed bundle");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mesh = net_sdk::MeshBuilder::new("127.0.0.1:0", bundle.psk().expose_bytes())
            .unwrap()
            .identity(join.identity().clone())
            .build()
            .await
            .map_err(|e| e.to_string())?;
        mesh.start();
        let c = bundle.contact();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            mesh.connect_via(&c.addr.unwrap().to_string(), &c.noise_pubkey, c.node_id),
        )
        .await;
        let _ = mesh.shutdown().await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("timed out".into()),
        }
    })
}

fn offer(status: &Value, offer_id: &str) -> Value {
    status["offers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["offer_id"] == offer_id)
        .cloned()
        .unwrap()
}

#[test]
fn up_enroll_refuses_only_unusable_explicit_inputs() {
    let fx = Fx::new();
    let issuer = fx.issuer("issuer.toml");
    let issuer_s = issuer.to_str().unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    let missing_ledger = fx.path("no-such-ledger");
    let cases: [(&str, &str, Vec<&str>); 2] = [
        (
            "explicit ledger that does not exist",
            "enrollment init",
            vec!["--ledger", missing_ledger.to_str().unwrap()],
        ),
        (
            "unusable public address",
            "--public-addr",
            vec!["--public-addr", "https://x:1"],
        ),
    ];
    for (what, reason, extra) in cases {
        let mut args = vec!["up", "--enroll", "--no-port-mapping", "--bind", &addr];
        args.extend(extra);
        let out = fx.run(&args);
        assert_eq!(out.status.code(), Some(2), "{what}: {out:?}");
        assert!(out.stdout.is_empty(), "{what}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(reason), "{what}: {err}");
        assert!(
            !fx.state().join("node").exists(),
            "{what} created node state"
        );
    }

    // An explicit issuer that does not own the default ledger is refused.
    assert!(fx.init(&fx.issuer("other.toml")).status.success());
    let out = fx.run(&[
        "up",
        "--enroll",
        "--no-port-mapping",
        "--bind",
        &addr,
        "--issuer-identity",
        issuer_s,
    ]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("different issuer"));
    assert_ne!(fx.json(&["node", "status"])["state"], "ready");

    // A second init never replaces a ledger; invite commands need a running node.
    assert_eq!(fx.init(&issuer).status.code(), Some(2));
    assert_eq!(fx.run(&["invite", "status"]).status.code(), Some(6));
}

#[test]
fn minimal_config_up_enroll_provisions_once_and_a_device_joins() {
    let fx = Fx::new();
    let start = |fx: &Fx| {
        let mut child = fx
            .base()
            .args(["--output", "ndjson", "up", "--enroll", "--no-port-mapping"])
            .args(["--bind", "127.0.0.1:0", "--state-dir"])
            .arg(fx.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = read_row(&mut stdout, &mut child);
        Up { child, ready }
    };

    // First run: nothing configured beyond a loopback bind.
    let first = start(&fx);
    let e = first.ready["enrollment"].clone();
    let endpoint = e["endpoint"].as_str().unwrap().to_string();
    assert!(
        endpoint.starts_with("127.0.0.1:") && !endpoint.ends_with(":0"),
        "{e}"
    );
    assert_eq!(e["port_mapping"], "disabled");
    let mut created: Vec<String> = serde_json::from_value(e["created"].clone()).unwrap();
    created.sort();
    assert_eq!(created, ["issuer", "ledger", "port"]);

    let created_invite = fx.json(&["invite", "create"]);
    let (status, device) = redeem(
        &fx.path("device"),
        created_invite["token"].as_str().unwrap(),
    );
    assert!(matches!(status.unwrap(), JoinStatus::Installed { .. }));
    attach(&device).expect("attach to the auto-provisioned node");
    fx.json(&["down"]);
    drop(first);

    // Restart: same port, issuer and ledger; nothing is created again.
    let second = start(&fx);
    let e2 = &second.ready["enrollment"];
    assert_eq!(e2["endpoint"], endpoint.as_str());
    assert_eq!(e2["issuer"], e["issuer"]);
    assert!(e2.get("created").is_none(), "{e2}");
    let row = offer(
        &fx.json(&["invite", "status"]),
        created_invite["offer_id"].as_str().unwrap(),
    );
    assert_eq!(row["state"], "issued");

    // A per-invite address overrides the default.
    let port = endpoint.rsplit_once(':').unwrap().1;
    let named = format!("localhost:{port}");
    let other = fx.json(&["invite", "create", "--addr", &named]);
    assert_eq!(other["endpoint"], named.as_str());
    let shown = MembershipInvite::decode(other["token"].as_str().unwrap()).unwrap();
    assert_eq!(shown.endpoint().unwrap().as_str(), named);
    fx.json(&["down"]);
    drop(second);
}

#[test]
fn a_clean_device_joins_the_up_node_through_a_printed_token() {
    let fx = Fx::new();
    let issuer = fx.issuer("issuer.toml");
    let init = fx.init(&issuer);
    assert!(init.status.success(), "{init:?}");
    let port = free_port();
    let up = fx.up_enroll(&issuer, port);
    let enrollment = &up.ready["enrollment"];
    assert_eq!(enrollment["endpoint"], format!("127.0.0.1:{port}"));
    assert_eq!(
        fx.json(&["node", "status"])["node"]["enrollment"]["issuer"],
        enrollment["issuer"]
    );

    // Default invitation: preauthorized, bearer, 24 hours.
    let created = fx.run(&["invite", "create"]);
    assert!(created.status.success(), "{created:?}");
    assert!(String::from_utf8_lossy(&created.stderr).contains("bearer"));
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    let token = created["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("netmesh-join_"));
    let offer_id = created["offer_id"].as_str().unwrap().to_string();

    // Offline inspection, from stdin so the token stays out of argv.
    let mut inspect = fx
        .base()
        .args(["--output", "json", "invite", "inspect", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    inspect
        .stdin
        .take()
        .unwrap()
        .write_all(token.as_bytes())
        .unwrap();
    let shown: Value = serde_json::from_slice(&inspect.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(shown["endpoint"], format!("127.0.0.1:{port}"));
    assert_eq!(
        shown["issuer_fingerprint"],
        enrollment["issuer_fingerprint"]
    );
    assert_eq!(shown["bearer"], true);
    assert_eq!(shown["approval"], "preauthorized");
    let ttl = shown["expires_at"].as_u64().unwrap() - shown["created_at"].as_u64().unwrap();
    assert_eq!(ttl, 86_400);
    assert!(!shown.to_string().contains(&token));

    // A clean device redeems it and attaches to the running node's mesh.
    let (status, device) = redeem(&fx.path("device1"), &token);
    assert!(matches!(status.unwrap(), JoinStatus::Installed { .. }));
    attach(&device).expect("attach with the delivered PSK");
    let status = fx.json(&["invite", "status", &offer_id]);
    let row = offer(&status, &offer_id);
    assert_eq!(row["state"], "issued");
    assert_eq!(
        row["subject"],
        hex::encode(device.identity().entity_id().as_bytes())
    );

    // The same token cannot enroll a second device.
    let (second, _) = redeem(&fx.path("device2"), &token);
    assert!(
        matches!(
            second,
            Err(DeviceJoinError::Redeem(RedeemError::Refused(
                Refusal::Conflict
            )))
        ),
        "{second:?}"
    );

    // A revoked, unredeemed token issues nothing.
    let revoked = fx.json(&["invite", "create"]);
    fx.json(&["invite", "revoke", revoked["offer_id"].as_str().unwrap()]);
    let (status, _) = redeem(&fx.path("device3"), revoked["token"].as_str().unwrap());
    assert!(
        matches!(
            status,
            Err(DeviceJoinError::Redeem(RedeemError::Refused(
                Refusal::Revoked
            )))
        ),
        "{status:?}"
    );
    drop(up);
}

#[test]
fn require_approval_needs_the_operator_to_approve_that_exact_device() {
    let fx = Fx::new();
    let issuer = fx.issuer("issuer.toml");
    assert!(fx.init(&issuer).status.success());
    let up = fx.up_enroll(&issuer, free_port());

    let created = fx.json(&["invite", "create", "--require-approval", "--ttl", "1h"]);
    assert_eq!(created["approval"], "require_approval");
    let (token, offer_id) = (
        created["token"].as_str().unwrap().to_string(),
        created["offer_id"].as_str().unwrap().to_string(),
    );
    let dir = fx.path("device");
    let (status, device) = redeem(&dir, &token);
    assert_eq!(status.unwrap(), JoinStatus::PendingApproval);
    let subject = hex::encode(device.identity().entity_id().as_bytes());
    let row = offer(&fx.json(&["invite", "status"]), &offer_id);
    assert_eq!(row["state"], "pending_approval");
    assert_eq!(row["subject"], subject);

    // Approving a subject other than the pending claimant is refused.
    let stranger = hex::encode(Identity::generate().entity_id().as_bytes());
    let wrong = fx.run(&["invite", "approve", &offer_id, "--subject", &stranger]);
    assert!(!wrong.status.success());
    assert_eq!(
        offer(&fx.json(&["invite", "status"]), &offer_id)["state"],
        "pending_approval"
    );

    fx.json(&["invite", "approve", &offer_id, "--subject", &subject]);
    drop(device);
    let mut device = DeviceJoin::open(&dir).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let status = rt.block_on(device.redeem(Duration::from_secs(10))).unwrap();
    assert!(matches!(status, JoinStatus::Installed { .. }));
    attach(&device).expect("attach after approval");
    drop(up);
}

#[test]
fn tokens_can_go_to_a_file_and_the_ledger_survives_restart() {
    let fx = Fx::new();
    let issuer = fx.issuer("issuer.toml");
    assert!(fx.init(&issuer).status.success());
    let port = free_port();
    let up = fx.up_enroll(&issuer, port);

    let file = fx.path("token.txt");
    let created = fx.json(&["invite", "create", "--out", file.to_str().unwrap()]);
    assert!(created["token"].is_null());
    let token = std::fs::read_to_string(&file).unwrap();
    assert!(MembershipInvite::decode(&token).is_ok());
    // Never overwrite an existing destination.
    assert_eq!(
        fx.run(&["invite", "create", "--out", file.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );

    fx.json(&["down"]);
    drop(up);
    assert_eq!(fx.run(&["invite", "status"]).status.code(), Some(6));

    let _up = fx.up_enroll(&issuer, port);
    let status = fx.json(&["invite", "status"]);
    let row = offer(&status, created["offer_id"].as_str().unwrap());
    assert_eq!(row["state"], "offered");
    fx.json(&["down"]);
}

#[test]
fn a_node_without_enroll_refuses_invite_operations() {
    let fx = Fx::new();
    let mut child = fx
        .base()
        .args([
            "--output",
            "ndjson",
            "up",
            "--bind",
            "127.0.0.1:0",
            "--state-dir",
        ])
        .arg(fx.state())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let ready = read_row(&mut stdout, &mut child);
    assert!(ready.get("enrollment").is_none(), "{ready}");
    let up = Up { child, ready };
    let out = fx.run(&["invite", "create"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not enabled"));
    fx.json(&["down"]);
    drop(up);
}
