//! Voluntary leave of a joined mesh (V3-2B, mesh relation): a running joined
//! node records the departure through its own control endpoint and stops; an
//! offline leave updates the join state directly. Either way the delivered
//! credentials are erased, restart stays left, and only an explicit
//! `join --rejoin` (back through the issuer) restores membership. Real
//! subprocesses on loopback with router mapping disabled.
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

/// A joined node that is running: `leave` goes through it, it stops, the
/// departure survives restart, and the operator is unaffected.
#[test]
fn leaving_stops_the_running_joined_node_and_restart_stays_left() {
    let operator = Fx::new();
    let _op = operator.operator();
    let token = token_of(&operator.json(&["invite", "create"]));
    let agent = Fx::new();
    let joined = agent.json(&["join", &token, "--yes"]);
    let device = joined["device"].clone();
    let node = agent.up(&[]);
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    let incarnation = node.ready["incarnation"].clone();

    let left = agent.json(&["leave"]);
    assert_eq!(left["state"], "left", "{left}");
    assert_eq!(left["newly_left"], true);
    assert_eq!(left["credentials"], "erased");
    assert_eq!(left["runtime"], "stopped");
    assert_eq!(left["incarnation"], incarnation);
    assert!(left["unmanaged_consumers"]
        .as_str()
        .unwrap()
        .starts_with("unknown"));
    assert!(left["authority"]
        .as_str()
        .unwrap()
        .starts_with("not notified"));
    assert_eq!(agent.json(&["node", "status"])["state"], "stopped");
    drop(node);

    // Restart does not rejoin.
    let out = agent.run(&["up", "--bind", "127.0.0.1:0"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("left the mesh"));

    // Repeated leave is idempotent and keeps the original time.
    let again = agent.json(&["leave"]);
    assert_eq!(again["newly_left"], false);
    assert_eq!(again["left_at"], left["left_at"]);
    assert_eq!(again["runtime"], "not_running");

    // The operator's node and its ledger are untouched by the device leaving.
    assert_eq!(operator.json(&["node", "status"])["state"], "ready");
    let offers = operator.json(&["invite", "status"]);
    assert_eq!(offers["offers"][0]["state"], "issued");
    assert_eq!(offers["offers"][0]["subject"], device);
}

/// Offline leave, then membership returns only through an explicit rejoin
/// that goes back to the issuer, keeping the same device identity.
#[test]
fn an_offline_leave_is_undone_only_by_an_explicit_rejoin() {
    let operator = Fx::new();
    let _op = operator.operator();
    let token = token_of(&operator.json(&["invite", "create"]));
    let agent = Fx::new();
    let device = agent.json(&["join", &token, "--yes"])["device"].clone();

    let left = agent.json(&["leave"]);
    assert_eq!(left["runtime"], "not_running", "{left}");

    // Re-running the same join does not silently undo the departure.
    let out = agent.run(&["join", &token, "--yes"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--rejoin"));

    let back = agent.json(&["join", &token, "--yes", "--rejoin"]);
    assert_eq!(back["state"], "joined", "{back}");
    assert_eq!(back["attached"], true);
    assert_eq!(
        back["device"], device,
        "the device identity survives leaving"
    );
    assert!(
        back["enroll_path"].is_string(),
        "rejoin redeems from the issuer: {back}"
    );
    let node = agent.up(&[]);
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
}

/// Nothing to leave: a state directory that never joined, and an operator's
/// own node, refuse without effect.
#[test]
fn leave_refuses_state_that_never_joined() {
    let fresh = Fx::new();
    let out = fresh.run(&["leave"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(!fresh.state().exists());

    let operator = Fx::new();
    let _op = operator.operator();
    let out = operator.run(&["leave"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(operator.json(&["node", "status"])["state"], "ready");
}
