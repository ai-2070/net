// SPDX-License-Identifier: MIT OR Apache-2.0
//! Crash recovery at the real durable-write transitions (NET_CLI_PLAN_V3
//! decision 7, E3/E7/E24). Built only with `--features fixtures`, which
//! compiles the core's crash points into `EnrollmentStorage` replacement:
//! `NET_MESH_FIXTURE_CRASH=<before|after>_replace:<store>:<nth>` aborts the
//! subprocess right there. Each test lets a real `net-mesh` process die at
//! that transition, then restarts against exactly what reached the disk.
#![cfg(feature = "fixtures")]

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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

    fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }

    fn base(&self, crash: Option<&str>) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .env_remove("NET_MESH_FIXTURE_CRASH")
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"));
        if let Some(spec) = crash {
            c.env("NET_MESH_FIXTURE_CRASH", spec);
        }
        c
    }

    fn exec(&self, args: &[&str], crash: Option<&str>) -> Output {
        let mut child = self
            .base(crash)
            .args(["--output", "json"])
            .args(args)
            .arg("--state-dir")
            .arg(self.state())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..800 {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        panic!(
            "{args:?} did not exit within 40s: {:?}",
            child.wait_with_output()
        );
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.exec(args, None);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn up(&self, crash: Option<&str>) -> Up {
        let mut child = self
            .base(crash)
            .args(["--output", "ndjson", "up", "--bind", &self.bind()])
            .args(["--enroll", "--no-port-mapping", "--no-relay"])
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
        Up { child }
    }

    /// A stable bind, so a restarted operator is reachable at the contact
    /// its earlier tokens and bundles name.
    fn bind(&self) -> String {
        let file = self.tmp.path().join("port");
        if let Ok(port) = std::fs::read_to_string(&file) {
            return format!("127.0.0.1:{port}");
        }
        let port = loop {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let port = udp.local_addr().unwrap().port();
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                break port;
            }
        };
        std::fs::write(&file, port.to_string()).unwrap();
        format!("127.0.0.1:{port}")
    }

    fn issued(&self) -> Vec<Value> {
        self.json(&["invite", "status"])["offers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r["state"] == "issued")
            .collect()
    }
}

struct Up {
    child: Child,
}

impl Up {
    /// Wait for the node to die on its own (the crash point).
    fn crashed(&mut self) -> std::process::ExitStatus {
        for _ in 0..400 {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the node did not reach its crash point");
    }
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

/// The operator dies right AFTER the ledger durably committed an issuance —
/// before any reply. On restart the device's retry recovers exactly that
/// committed issuance: one issued record, the same receipt, no second grant.
#[test]
fn an_operator_crash_after_the_issuance_commit_is_recovered_not_reissued() {
    let operator = Fx::new();
    let first = operator.up(None);
    drop(first);
    // The crashing run's ledger writes (read off `NET_MESH_FIXTURE_TRACE`):
    // its startup write, the offer, the claim, then the issuance — the 4th.
    let mut crashing = operator.up(Some("after_replace:ledger:4"));
    let created = operator.json(&["invite", "create"]);
    let token = created["token"].as_str().unwrap().to_string();
    let device = Fx::new();
    let failed = device.exec(&["join", &token, "--yes"], None);
    assert!(!failed.status.success(), "the operator died mid-redemption");
    assert!(!crashing.crashed().success(), "aborted at the crash point");
    drop(crashing);

    let _restarted = operator.up(None);
    let issued = operator.issued();
    assert_eq!(
        issued.len(),
        1,
        "the issuance was committed before the crash"
    );
    let receipt = issued[0]["receipt_id"].clone();
    let joined = device.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    let issued = operator.issued();
    assert_eq!(issued.len(), 1, "no second issuance");
    assert_eq!(
        issued[0]["receipt_id"], receipt,
        "the committed receipt, recovered"
    );
}

/// The operator dies right BEFORE the issuance write reaches the disk: the
/// claim is durable, the issuance is not. The device's retry resumes its own
/// claim and is issued exactly once.
#[test]
fn an_operator_crash_before_the_issuance_commit_issues_exactly_once_on_retry() {
    let operator = Fx::new();
    let first = operator.up(None);
    drop(first);
    let mut crashing = operator.up(Some("before_replace:ledger:4"));
    let created = operator.json(&["invite", "create"]);
    let token = created["token"].as_str().unwrap().to_string();
    let device = Fx::new();
    assert!(!device
        .exec(&["join", &token, "--yes"], None)
        .status
        .success());
    assert!(!crashing.crashed().success());
    drop(crashing);

    let _restarted = operator.up(None);
    assert!(
        operator.issued().is_empty(),
        "nothing was issued before the crash"
    );
    // Another device cannot take the claim the first one committed.
    let thief = Fx::new();
    assert!(!thief
        .exec(&["join", &token, "--yes"], None)
        .status
        .success());
    let joined = device.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(operator.issued().len(), 1);
}

/// The DEVICE dies right after its join store durably installed the
/// delivered bundle — before it attached or reported anything. Re-running
/// `join` resumes from the installed state: same device identity, no second
/// redemption at the operator.
#[test]
fn a_device_crash_after_installing_its_bundle_resumes_without_redeeming_again() {
    let operator = Fx::new();
    let _op = operator.up(None);
    let created = operator.json(&["invite", "create"]);
    let token = created["token"].as_str().unwrap().to_string();
    let device = Fx::new();
    // The join store's writes: the intent before redemption, then the
    // installed bundle — the 2nd.
    let crashed = device.exec(&["join", &token, "--yes"], Some("after_replace:join:2"));
    assert!(!crashed.status.success(), "{crashed:?}");
    assert!(
        String::from_utf8_lossy(&crashed.stderr).contains("fixture crash"),
        "{crashed:?}"
    );
    let issued = operator.issued();
    assert_eq!(issued.len(), 1, "the operator issued once");
    let joined = device.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(
        joined["device"], issued[0]["subject"],
        "the same device identity"
    );
    assert!(
        joined["enroll_path"].is_null(),
        "installed before: no redemption ran"
    );
    assert_eq!(operator.issued().len(), 1, "no second issuance");
}
