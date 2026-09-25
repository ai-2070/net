// SPDX-License-Identifier: MIT OR Apache-2.0
//! Organization enrollment through the CLI (V3-2 org O1): an org invite is
//! always approval-gated; `org approve --org-key` signs the membership for
//! exactly the claiming device on the operator's machine (the org root never
//! reaches a node); the device adopts it and its `up` installs it. A
//! standalone org link does the same for a device already on the mesh, over
//! its session, completing by itself once approved. Real subprocesses on
//! loopback with router mapping disabled.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
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

/// A fresh org root key file and its org id.
fn org_keygen(dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let out = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["--output", "json", "org", "keygen", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    (key, v["org_id_hex"].as_str().unwrap().to_string())
}

/// Poll `node status` until `done` holds.
fn wait_for_status(fx: &Fx, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..180 {
        last = fx.json(&["node", "status"]);
        if done(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("{what}: never got there; last status {last}");
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn a_device_joins_an_org_only_with_the_operators_root_signed_approval() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (org_key, org) = org_keygen(&keys, "org.toml");
    let (other_key, _) = org_keygen(&keys, "other-org.toml");
    let _op = operator.up(&["--enroll", "--no-port-mapping"]);

    // An org invite: always approval-gated.
    let created = operator.json(&["invite", "create", "--org", &org]);
    assert_eq!(created["org"], org.as_str(), "{created}");
    assert_eq!(created["approval"], "require_approval", "{created}");
    let offer = created["offer_id"].as_str().unwrap().to_string();

    let agent = Fx::new();
    let pending = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(pending["state"], "pending_approval", "{pending}");
    let device = pending["device"].as_str().unwrap().to_string();

    // Plain approval cannot issue an org membership: only the org root can.
    let plain = operator.run(&["invite", "approve", &offer, "--subject", &device]);
    assert!(!plain.status.success(), "{plain:?}");
    assert!(stderr_of(&plain).contains("org approve"), "{plain:?}");
    // The wrong org's root, or the wrong device, is refused.
    let wrong_org = operator.run(&[
        "org",
        "approve",
        &offer,
        "--subject",
        &device,
        "--org-key",
        other_key.to_str().unwrap(),
    ]);
    assert!(!wrong_org.status.success(), "{wrong_org:?}");
    assert!(
        stderr_of(&wrong_org).contains("but the invite offers org"),
        "{wrong_org:?}"
    );
    let stranger = "ab".repeat(32);
    let wrong_device = operator.run(&[
        "org",
        "approve",
        &offer,
        "--subject",
        &stranger,
        "--org-key",
        org_key.to_str().unwrap(),
    ]);
    assert!(!wrong_device.status.success(), "{wrong_device:?}");

    // The operator signs, here, for exactly this device.
    // The org's shared audience, minted once, rides along with the approval.
    let audience = keys.join("org.audience");
    let minted = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["--output", "json", "org", "audience-keygen", "--org-key"])
        .arg(&org_key)
        .arg("--out")
        .arg(&audience)
        .output()
        .unwrap();
    assert!(minted.status.success(), "{minted:?}");
    let minted: Value = serde_json::from_slice(&minted.stdout).unwrap();
    assert_eq!(minted["org"], org.as_str(), "{minted}");
    let approved = operator.json(&[
        "org",
        "approve",
        &offer,
        "--subject",
        &device,
        "--org-key",
        org_key.to_str().unwrap(),
        "--audience",
        audience.to_str().unwrap(),
    ]);
    assert_eq!(approved["state"], "approved", "{approved}");
    assert_eq!(approved["member"], device.as_str());
    assert_eq!(approved["audience"], true, "{approved}");

    // The device's join now completes and adopts the membership.
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(joined["state"], "joined", "{joined}");
    assert_eq!(joined["org"]["org"], org.as_str(), "{joined}");
    assert_eq!(joined["org"]["adopted"], true, "{joined}");

    // The device adopted the org's audience, byte for byte.
    assert_eq!(joined["org"]["audience"], "org", "{joined}");
    assert_eq!(
        std::fs::read(agent.state().join("authority").join("owner-audience.key")).unwrap(),
        std::fs::read(&audience).unwrap()
    );

    // Its `up` installs the membership; status reports it live.
    let node = agent.up(&[]);
    assert_eq!(node.ready["org"], org.as_str(), "{}", node.ready);
    assert_eq!(agent.json(&["node", "status"])["org"], org.as_str());
    drop(node);
}

#[test]
fn a_device_already_on_the_mesh_joins_an_org_with_a_standalone_link() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (org_key, org) = org_keygen(&keys, "org.toml");
    let _op = operator.up(&["--enroll", "--no-port-mapping"]);

    // A device joined the mesh only: no org yet.
    let created = operator.json(&["invite", "create"]);
    let agent = Fx::new();
    let joined = agent.json(&["join", &token_of(&created), "--yes"]);
    let device = joined["device"].as_str().unwrap().to_string();
    let node = agent.up(&[]);
    assert!(node.ready["org"].is_null(), "{}", node.ready);

    // A mesh invite is not a standalone org link.
    let refused = agent.run(&[
        "org",
        "join",
        &token_of(&operator.json(&["invite", "create"])),
        "--yes",
    ]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        stderr_of(&refused).contains("not a standalone org link"),
        "{refused:?}"
    );

    // The standalone link waits for the operator.
    let link = operator.json(&["org", "invite", &org]);
    assert_eq!(link["standalone"], true, "{link}");
    let pending = agent.json(&["org", "join", &token_of(&link), "--yes"]);
    assert_eq!(pending["state"], "pending_approval", "{pending}");
    assert_eq!(
        pending["device"],
        device.as_str(),
        "the same proven identity"
    );
    assert_eq!(agent.json(&["node", "status"])["org_pending"], 1);

    // The pending link survives a restart of the device's node.
    drop(node);
    let node = agent.up(&[]);
    assert_eq!(agent.json(&["node", "status"])["org_pending"], 1);

    // Approved: the running node completes it by itself and installs it.
    let offer = link["offer_id"].as_str().unwrap().to_string();
    operator.json(&[
        "org",
        "approve",
        &offer,
        "--subject",
        &device,
        "--org-key",
        org_key.to_str().unwrap(),
    ]);
    wait_for_status(&agent, "org membership installed", |s| {
        s["org"] == org.as_str() && s["org_pending"] == 0
    });

    // Adopted durably: the next start installs it again.
    drop(node);
    let node = agent.up(&[]);
    assert_eq!(node.ready["org"], org.as_str(), "{}", node.ready);
    drop(node);
}

/// `ENTITY@HOST:PORT#NOISE_PUBKEY` for a running node's ready row.
fn contact_of(ready: &Value) -> String {
    format!(
        "{}@{}#{}",
        ready["entity_id"].as_str().unwrap(),
        ready["bind"].as_str().unwrap(),
        ready["public_key"].as_str().unwrap()
    )
}

/// O2 end to end: the operator (itself an org member, adopted with
/// `node adopt`) removes an enrolled device with `org remove`. Each named node
/// answers with its own signed attestation: the operator's node and the
/// device's apply the floor; a mesh-only bystander attests it enforces no org
/// (so `complete` is false: nothing is claimed for it). The device keeps
/// running on the mesh without the org across restart (its membership is
/// reported revoked, not a startup failure), and the operator's floor
/// survives its own restart.
#[test]
fn org_remove_applies_a_root_signed_floor_at_each_named_node() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (org_key, org) = org_keygen(&keys, "org.toml");
    let audience = keys.join("org.audience");
    let minted = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["org", "audience-keygen", "--org-key"])
        .arg(&org_key)
        .arg("--out")
        .arg(&audience)
        .output()
        .unwrap();
    assert!(minted.status.success(), "{minted:?}");

    // The operator's node becomes an org member itself: first start to learn
    // its entity, then issue and adopt its membership, then start again.
    let op = operator.up(&["--enroll", "--no-port-mapping"]);
    let op_entity = op.ready["entity_id"].as_str().unwrap().to_string();
    drop(op);
    let cert = keys.join("operator-cert.json");
    let issued = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .args(["org", "issue-cert", "--org-key"])
        .arg(&org_key)
        .args(["--member", &op_entity, "--out"])
        .arg(&cert)
        .output()
        .unwrap();
    assert!(issued.status.success(), "{issued:?}");
    let adopted = operator
        .base()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", &op_entity, "--authority-dir"])
        .arg(operator.state().join("authority"))
        .arg("--audience")
        .arg(&audience)
        .output()
        .unwrap();
    assert!(adopted.status.success(), "{adopted:?}");
    assert_eq!(
        std::fs::read(
            operator
                .state()
                .join("authority")
                .join("owner-audience.key")
        )
        .unwrap(),
        std::fs::read(&audience).unwrap()
    );
    let op = operator.up(&["--enroll", "--no-port-mapping"]);
    assert_eq!(op.ready["org"], org.as_str(), "{}", op.ready);

    // An enrolled member device.
    let created = operator.json(&["invite", "create", "--org", &org]);
    let offer = created["offer_id"].as_str().unwrap().to_string();
    let agent = Fx::new();
    let pending = agent.json(&["join", &token_of(&created), "--yes"]);
    let device = pending["device"].as_str().unwrap().to_string();
    operator.json(&[
        "org",
        "approve",
        &offer,
        "--subject",
        &device,
        "--org-key",
        org_key.to_str().unwrap(),
    ]);
    agent.json(&["join", &token_of(&created), "--yes"]);
    let node = agent.up(&[]);
    assert_eq!(node.ready["org"], org.as_str(), "{}", node.ready);

    // A mesh-only bystander.
    let bystander = Fx::new();
    bystander.json(&[
        "join",
        &token_of(&operator.json(&["invite", "create"])),
        "--yes",
    ]);
    let other = bystander.up(&[]);

    let remove = |verifiers: &[String]| {
        let mut args: Vec<String> = [
            "org",
            "remove",
            &device,
            "--org-key",
            org_key.to_str().unwrap(),
            "--minimum-generation",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for v in verifiers {
            args.push("--verifier".into());
            args.push(v.clone());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        operator.json(&refs)
    };

    // A dry run signs and sends nothing.
    let dry = operator.json(&[
        "org",
        "remove",
        &device,
        "--org-key",
        org_key.to_str().unwrap(),
        "--minimum-generation",
        "1",
        "--verifier",
        "self",
        "--dry-run",
    ]);
    assert_eq!(dry["dry_run"], true, "{dry}");

    // Before removal: the device is issued, and admissible at the operator.
    let members = operator.json(&["org", "members", &org]);
    assert_eq!(
        members["issued"][0]["subject"],
        device.as_str(),
        "{members}"
    );
    assert_eq!(members["issued"][0]["state"], "issued", "{members}");
    assert_eq!(members["issued"][0]["approved_generation"], 0, "{members}");
    assert_eq!(members["observed"]["enforces_this_org"], true, "{members}");
    assert_eq!(
        members["observed"]["standing"][0]["standing"], "admissible_here",
        "{members}"
    );

    let removed = remove(&[
        "self".to_string(),
        contact_of(&node.ready),
        contact_of(&other.ready),
    ]);
    let rows = removed["verifiers"].as_array().unwrap();
    assert_eq!(rows[0]["state"], "applied", "{removed}");
    assert_eq!(rows[0]["floor"], 1, "{removed}");
    assert_eq!(rows[1]["state"], "applied", "{removed}");
    assert_eq!(rows[2]["state"], "not_member", "{removed}");
    assert_eq!(removed["applied"], 2, "{removed}");
    assert_eq!(removed["complete"], false, "the bystander enforces nothing");
    // Signed observations, asked with the org root: the operator's node and
    // the device's own node both hold the floor now; the bystander enforces
    // no org.
    let observed = operator.json(&[
        "org",
        "members",
        &org,
        "--verifier",
        "self",
        "--verifier",
        &contact_of(&node.ready),
        "--verifier",
        &contact_of(&other.ready),
        "--org-key",
        org_key.to_str().unwrap(),
    ]);
    let rows = observed["remote"].as_array().unwrap();
    assert_eq!(rows[0]["state"], "observed", "{observed}");
    assert_eq!(
        rows[0]["standing"][0]["standing"], "revoked_there",
        "{observed}"
    );
    assert_eq!(rows[0]["standing"][0]["floor"], 1, "{observed}");
    assert_eq!(rows[1]["state"], "observed", "{observed}");
    assert_eq!(
        rows[1]["standing"][0]["standing"], "revoked_there",
        "{observed}"
    );
    assert_eq!(rows[2]["state"], "not_member", "{observed}");
    // After removal: still issued, but revoked here (floor above the
    // generation the operator signed).
    let members = operator.json(&["org", "members", &org]);
    assert_eq!(members["issued"][0]["state"], "issued", "{members}");
    assert_eq!(
        members["observed"]["standing"][0]["floor_here"], 1,
        "{members}"
    );
    assert_eq!(
        members["observed"]["standing"][0]["standing"], "revoked_here",
        "{members}"
    );

    // The device runs on without the org: revoked, not a startup failure.
    drop(node);
    let node = agent.up(&[]);
    assert!(node.ready["org"].is_null(), "{}", node.ready);
    assert_eq!(
        node.ready["org_state"]["state"], "revoked",
        "{}",
        node.ready
    );
    drop(node);
    // An ended membership is tolerated; a corrupt authority is not.
    let membership = agent
        .state()
        .join("authority")
        .join("owner-membership.json");
    std::fs::write(&membership, b"{ not json").unwrap();
    let broken = agent.run(&["up", "--bind", "127.0.0.1:0"]);
    assert!(!broken.status.success(), "{broken:?}");
    assert!(stderr_of(&broken).contains("org authority"), "{broken:?}");
    drop(other);

    // The operator's floor was persisted: after its restart it still holds.
    drop(op);
    let _op = operator.up(&["--enroll", "--no-port-mapping"]);
    let again = remove(&["self".to_string()]);
    assert_eq!(again["verifiers"][0]["state"], "applied", "{again}");
    assert_eq!(again["verifiers"][0]["floor"], 1, "{again}");
    assert_eq!(again["complete"], true, "{again}");
}

/// `org leave`: recorded durably, then the running node stops; its next
/// start runs on the mesh without the org (reported `left`). Re-running the
/// original join token does not restore the membership; a new link approved
/// with the org root does, live, and it holds across restart. Offline, the
/// departure is recorded directly.
#[test]
fn org_leave_holds_until_an_approved_rejoin() {
    let operator = Fx::new();
    let keys = operator.tmp.path().join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let (org_key, org) = org_keygen(&keys, "org.toml");
    let _op = operator.up(&["--enroll", "--no-port-mapping"]);
    let approve = |offer: &str, device: &str| {
        operator.json(&[
            "org",
            "approve",
            offer,
            "--subject",
            device,
            "--org-key",
            org_key.to_str().unwrap(),
        ])
    };

    let created = operator.json(&["invite", "create", "--org", &org]);
    let agent = Fx::new();
    let pending = agent.json(&["join", &token_of(&created), "--yes"]);
    let device = pending["device"].as_str().unwrap().to_string();
    approve(created["offer_id"].as_str().unwrap(), &device);
    agent.json(&["join", &token_of(&created), "--yes"]);
    let node = agent.up(&[]);
    assert_eq!(node.ready["org"], org.as_str(), "{}", node.ready);

    // Leave: recorded, and the running node stops.
    let left = agent.json(&["org", "leave"]);
    assert_eq!(left["state"], "left", "{left}");
    assert_eq!(left["newly_left"], true, "{left}");
    assert_eq!(left["org"], org.as_str(), "{left}");
    assert_eq!(left["runtime"], "stopped", "{left}");
    assert_eq!(agent.json(&["node", "status"])["state"], "stopped");
    drop(node);

    // The next start: on the mesh, without the org.
    let node = agent.up(&[]);
    assert!(node.ready["org"].is_null(), "{}", node.ready);
    assert_eq!(node.ready["org_state"]["state"], "left", "{}", node.ready);
    assert_eq!(node.ready["joined"]["attached"], true, "{}", node.ready);
    drop(node);

    // Re-running the original token does not restore the membership.
    let again = agent.json(&["join", &token_of(&created), "--yes"]);
    assert_eq!(again["org"]["state"], "left", "{again}");
    let node = agent.up(&[]);
    assert!(node.ready["org"].is_null(), "{}", node.ready);

    // A new link, approved with the org root, rejoins — live.
    let link = operator.json(&["org", "invite", &org]);
    let rejoin = agent.json(&["org", "join", &token_of(&link), "--yes"]);
    assert_eq!(rejoin["state"], "pending_approval", "{rejoin}");
    approve(link["offer_id"].as_str().unwrap(), &device);
    wait_for_status(&agent, "rejoined org installed", |s| {
        s["org"] == org.as_str()
    });
    drop(node);
    let node = agent.up(&[]);
    assert_eq!(node.ready["org"], org.as_str(), "{}", node.ready);
    drop(node);

    // Offline: the departure is recorded directly, and holds at the next start.
    let offline = agent.json(&["org", "leave"]);
    assert_eq!(offline["runtime"], "not running", "{offline}");
    assert_eq!(offline["newly_left"], true, "{offline}");
    let node = agent.up(&[]);
    assert_eq!(node.ready["org_state"]["state"], "left", "{}", node.ready);
    drop(node);
}
