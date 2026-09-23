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

const PSK_HEX: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

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
    std::fs::write(&psk, PSK_HEX).unwrap();

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

    // Remove the device from 3.7 at the operator's node, attested by it.
    let contact = format!(
        "{}@{}#{}",
        op.ready["entity_id"].as_str().unwrap(),
        op.ready["bind"].as_str().unwrap(),
        op.ready["public_key"].as_str().unwrap()
    );
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
        .args(["--verifier", &contact, "--psk-hex", PSK_HEX])
        .output()
        .unwrap();
    assert!(removed.status.success(), "{removed:?}");
    let removed: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed["verifiers"][0]["state"], "applied", "{removed}");
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
