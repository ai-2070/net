//! Relay fallback through the CLI (R2 phase 3): an operator runs `up --enroll
//! --relay`, and an agent's `join` and joined `up` try the direct path first and
//! fall back to the blind relay only when the direct path is dead. Real
//! subprocesses on loopback with router mapping disabled; a real `relay serve`
//! process. NAT evidence lives in natsim.
use std::io::{BufRead, BufReader, Read, Write};
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

struct Relay(Child);

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_relay() -> (Relay, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .args([
            "--output",
            "ndjson",
            "relay",
            "serve",
            "--bind",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let ready = read_row(&mut stdout, &mut child);
    assert_eq!(ready["event"], "ready", "{ready}");
    let addr = ready["relay"].as_str().unwrap().to_string();
    (Relay(child), addr)
}

/// A loopback port nothing listens on (TCP refuses, UDP goes nowhere).
fn dead_port() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

/// Direct dead, relay alive: enrollment and both attaches complete through
/// the relay automatically, with no flag on the joiner's side.
#[test]
fn a_dead_direct_path_falls_back_to_the_relay() {
    let (_relay, relay) = start_relay();
    let operator = Fx::new();
    let op = operator.up(&["--enroll", "--no-port-mapping", "--relay", &relay]);
    assert_eq!(
        op.ready["enrollment"]["relay"],
        relay.as_str(),
        "{}",
        op.ready
    );
    assert_eq!(
        op.ready["enrollment"]["relay_state"], "registered",
        "{}",
        op.ready
    );

    let dead = dead_port();
    let created = operator.json(&["invite", "create", "--addr", &dead]);
    assert_eq!(created["endpoint"], dead.as_str());
    assert_eq!(created["relay"], relay.as_str());

    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(joined["attached"], true);
    assert_eq!(joined["enroll_path"], "relay", "{joined}");
    assert_eq!(joined["attach_path"], "relay", "{joined}");
    assert_eq!(joined["contact"], dead.as_str());
    assert_eq!(joined["relay"], relay.as_str());

    let node = agent.up(&[]);
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    assert_eq!(node.ready["joined"]["path"], "relay", "{}", node.ready);
    drop(node);
    drop(op);
}

/// Direct alive, relay alive: the direct path is used and the relay is not.
#[test]
fn a_live_direct_path_is_used_while_a_relay_is_present() {
    let (_relay, relay) = start_relay();
    let operator = Fx::new();
    let _op = operator.up(&["--enroll", "--no-port-mapping", "--relay", &relay]);
    let created = operator.json(&["invite", "create"]);
    assert_eq!(created["relay"], relay.as_str());

    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["enroll_path"], "direct", "{joined}");
    assert_eq!(joined["attach_path"], "direct", "{joined}");
}

/// Relay dead, direct alive: relay availability is not a prerequisite. The
/// node still starts (reporting the relay unavailable), tokens still carry
/// it, and joins go direct.
#[test]
fn a_dead_relay_does_not_block_the_direct_path() {
    let dead_relay = dead_port();
    let operator = Fx::new();
    let op = operator.up(&["--enroll", "--no-port-mapping", "--relay", &dead_relay]);
    assert_eq!(
        op.ready["enrollment"]["relay_state"], "unavailable",
        "{}",
        op.ready
    );
    let created = operator.json(&["invite", "create"]);
    assert_eq!(created["relay"], dead_relay.as_str());

    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["enroll_path"], "direct", "{joined}");
    assert_eq!(joined["attach_path"], "direct", "{joined}");
}

#[test]
fn relay_flags_are_validated_before_any_effect() {
    let fx = Fx::new();
    let out = fx.run(&[
        "up",
        "--enroll",
        "--no-port-mapping",
        "--relay",
        "not a relay",
    ]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = fx.run(&["up", "--relay", "relay.example:7000"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "--relay requires --enroll: {out:?}"
    );
    assert!(!fx.state().exists());
}
