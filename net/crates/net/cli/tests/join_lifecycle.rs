//! The CLI-only journey: an operator runs `up --enroll` and `invite create`; an
//! agent runs `join <token>` and then `up` as the joined node. Real
//! subprocesses on loopback with router mapping disabled, so a developer's own
//! router is never touched. Multi-host / NAT evidence lives in natsim.
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

    /// Operator node with minimal-config enrollment.
    fn operator(&self) -> Up {
        self.up(&["--enroll", "--no-port-mapping"])
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

#[test]
fn an_agent_joins_with_the_cli_and_runs_as_the_joined_node() {
    let operator = Fx::new();
    let op = operator.operator();
    let token = token_of(&operator.json(&["invite", "create"]));
    let agent = Fx::new();

    // No confirmation, no effect: non-interactive without --yes refuses (8)
    // before anything is written, including when the token comes on stdin.
    let refused = agent.run(&["join", &token]);
    assert_eq!(refused.status.code(), Some(8), "{refused:?}");
    let refused = agent.run_with(&["join", "-"], Some(&token));
    assert_eq!(refused.status.code(), Some(8), "{refused:?}");
    assert!(!agent.state().exists());

    // The confirmation summary names the issuer before anything happens.
    let joined = agent.run_with(&["join", "-", "--yes"], Some(&token));
    assert!(joined.status.success(), "{joined:?}");
    let summary = String::from_utf8_lossy(&joined.stderr);
    let fingerprint = op.ready["enrollment"]["issuer_fingerprint"]
        .as_str()
        .unwrap();
    assert!(summary.contains(fingerprint), "{summary}");
    let joined: Value = serde_json::from_slice(&joined.stdout).unwrap();
    assert_eq!(joined["state"], "joined");
    assert_eq!(joined["attached"], true);
    assert_eq!(joined["issuer_fingerprint"], fingerprint);
    assert_eq!(joined["trust_domain"], op.ready["trust_domain"]);

    // Re-running the same join is idempotent and issues nothing new.
    let again = agent.json(&["join", &token, "--yes"]);
    assert_eq!(again["device"], joined["device"]);
    let offers = operator.json(&["invite", "status"]);
    assert_eq!(offers["offers"].as_array().unwrap().len(), 1);
    assert_eq!(offers["offers"][0]["state"], "issued");

    // A second agent cannot use the same bearer token.
    let other = Fx::new();
    let out = other.run(&["join", &token, "--yes"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("refused"));

    // The joined agent runs as a node of the operator's mesh.
    let node = agent.up(&[]);
    assert_eq!(node.ready["psk_source"], "joined");
    assert_eq!(node.ready["trust_domain"], op.ready["trust_domain"]);
    assert_eq!(node.ready["entity_id"], joined["device"]);
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    agent.json(&["down"]);
    drop(node);

    // A joined node never becomes an enrollment owner for someone else's mesh.
    let out = agent.run(&["up", "--enroll", "--bind", "127.0.0.1:0"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    operator.json(&["down"]);
    drop(op);
}

#[test]
fn a_require_approval_token_joins_only_after_the_operator_approves() {
    let operator = Fx::new();
    let op = operator.operator();
    let created = operator.json(&["invite", "create", "--require-approval"]);
    let token = token_of(&created);
    let agent = Fx::new();

    let pending = agent.json(&["join", &token, "--yes"]);
    assert_eq!(pending["state"], "pending_approval");
    // A pending join cannot start a node.
    let out = agent.run(&["up", "--bind", "127.0.0.1:0"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("pending approval"));

    let offer = created["offer_id"].as_str().unwrap();
    let device = pending["device"].as_str().unwrap();
    operator.json(&["invite", "approve", offer, "--subject", device]);
    let joined = agent.json(&["join", &token, "--yes"]);
    assert_eq!(joined["state"], "joined");
    assert_eq!(joined["device"], device);
    operator.json(&["down"]);
    drop(op);
}

#[test]
fn a_state_dir_holding_one_join_refuses_a_different_token() {
    let operator = Fx::new();
    let op = operator.operator();
    let first = token_of(&operator.json(&["invite", "create"]));
    let second = token_of(&operator.json(&["invite", "create"]));
    let agent = Fx::new();
    agent.json(&["join", &first, "--yes"]);
    let out = agent.run(&["join", &second, "--yes"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("different token"));
    operator.json(&["down"]);
    drop(op);
}

#[test]
fn installed_and_attached_are_reported_separately() {
    let operator = Fx::new();
    let op = operator.operator();
    let token = token_of(&operator.json(&["invite", "create"]));
    let agent = Fx::new();
    assert_eq!(agent.json(&["join", &token, "--yes"])["attached"], true);
    operator.json(&["down"]);
    drop(op);

    // Credentials stay installed (no network needed to know that), but with
    // the issuer's node gone the live attach must fail and say so.
    let out = agent.run(&["join", &token, "--yes", "--wait", "2s"]);
    assert_eq!(out.status.code(), Some(6), "{out:?}");
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("credentials are installed"), "{err}");

    // `up` still starts from the installed join and reports the failed attach.
    let node = agent.up(&[]);
    assert_eq!(node.ready["psk_source"], "joined");
    assert_eq!(node.ready["joined"]["attached"], false, "{}", node.ready);
    agent.json(&["down"]);
    drop(node);
}
