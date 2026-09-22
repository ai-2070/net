//! `net-mesh up` / `down` / `node status` as real subprocesses: one foreground
//! production node per state directory, liveness from its lifetime lock, and a
//! mutually authenticated loopback control endpoint. Loopback, one host.
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

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_net-mesh"));
        c.env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(self.tmp.path().join("config.toml"))
            .args(args)
            .arg("--state-dir")
            .arg(self.state());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut full = vec!["--output", "json"];
        full.extend_from_slice(args);
        self.cmd(&full).output().unwrap()
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

    fn status(&self) -> Value {
        self.json(&["node", "status"])
    }

    /// Start `up` and wait for its readiness row.
    fn up(&self, extra: &[&str]) -> Up {
        self.up_with_stdin(extra, None)
    }

    fn up_with_stdin(&self, extra: &[&str], stdin: Option<&[u8]>) -> Up {
        let mut args = vec!["--output", "ndjson", "up", "--bind", "127.0.0.1:0"];
        args.extend_from_slice(extra);
        let mut cmd = self.cmd(&args);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = cmd.spawn().unwrap();
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(bytes).unwrap();
        }
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = read_row(&mut stdout, &mut child);
        assert_eq!(ready["event"], "ready", "{ready}");
        Up {
            child,
            stdout,
            ready,
        }
    }
}

struct Up {
    child: Child,
    stdout: BufReader<ChildStdout>,
    ready: Value,
}

impl Up {
    fn wait_exit(&mut self) -> std::process::ExitStatus {
        for _ in 0..200 {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("up did not exit");
    }

    fn next_row(&mut self) -> Value {
        read_row(&mut self.stdout, &mut self.child)
    }
}

impl Drop for Up {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one ndjson row with a deadline; on failure include the child's stderr.
fn read_row(stdout: &mut BufReader<ChildStdout>, child: &mut Child) -> Value {
    let (tx, rx) = mpsc::channel();
    // Read on a scoped thread so a hung child cannot hang the test forever.
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

fn control_port(state: &Path) -> u16 {
    let text = std::fs::read_to_string(state.join("node").join("control.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    v["port"].as_u64().unwrap() as u16
}

#[test]
fn up_status_down_and_restart_keep_identity_and_generated_trust_domain() {
    let fx = Fx::new();
    let mut first = fx.up(&[]);
    let ready = first.ready.clone();
    assert_eq!(ready["psk_source"], "generated");

    let status = fx.status();
    assert_eq!(status["state"], "ready", "{status}");
    assert_eq!(status["node"]["incarnation"], ready["incarnation"]);
    assert_eq!(status["node"]["entity_id"], ready["entity_id"]);

    // A second owner for the same state directory is refused, naming the live one.
    let dup = fx.run(&["up", "--bind", "127.0.0.1:0"]);
    assert!(!dup.status.success());
    let err = String::from_utf8_lossy(&dup.stderr);
    assert!(err.contains("already running"), "{err}");
    assert!(
        err.contains(ready["incarnation"].as_str().unwrap()),
        "{err}"
    );

    let down = fx.json(&["down"]);
    assert_eq!(down["was_running"], true);
    assert_eq!(down["incarnation"], ready["incarnation"]);
    // `down` returns only once the lifetime lock is released: already stopped.
    assert_eq!(fx.status()["state"], "stopped");
    assert!(first.wait_exit().success());
    assert_eq!(first.next_row()["event"], "stopped");
    assert!(!fx.state().join("node").join("control.json").exists());

    // Restart: same identity and generated trust domain, new incarnation.
    let second = fx.up(&[]);
    assert_eq!(second.ready["entity_id"], ready["entity_id"]);
    assert_eq!(second.ready["trust_domain"], ready["trust_domain"]);
    assert_ne!(second.ready["incarnation"], ready["incarnation"]);
    fx.json(&["down"]);

    // Stopping an already-stopped node is a no-op success.
    let again = fx.json(&["down"]);
    assert_eq!(again["was_running"], false);
    drop(second);
}

#[test]
fn a_killed_node_leaves_stale_metadata_that_never_reads_as_running() {
    let fx = Fx::new();
    let mut up = fx.up(&[]);
    let first = up.ready["incarnation"].clone();
    up.child.kill().unwrap();
    up.child.wait().unwrap();

    let status = fx.status();
    assert_eq!(status["state"], "stale_metadata", "{status}");
    let down = fx.json(&["down"]);
    assert_eq!(down["was_running"], false);
    assert_eq!(down["stale_metadata"], true);

    // The OS released the lifetime lock; a new owner starts cleanly.
    let restarted = fx.up(&[]);
    assert_ne!(restarted.ready["incarnation"], first);
    assert_eq!(fx.status()["state"], "ready");
    fx.json(&["down"]);
}

#[test]
fn file_and_stdin_psk_sources_supply_the_exact_trust_domain_without_echoing_it() {
    let fx = Fx::new();
    let psk = [0x42u8; 32];
    let hex_psk = hex::encode(psk);
    let expected = net_sdk::bootstrap_credential::Psk::new(psk)
        .trust_domain()
        .to_string();

    let file = fx.tmp.path().join("psk.hex");
    std::fs::write(&file, format!("{hex_psk}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = format!("file:{}", file.display());
    let from_file = fx.up(&["--psk-from", &source]);
    assert_eq!(from_file.ready["psk_source"], "file");
    assert_eq!(from_file.ready["trust_domain"], expected.as_str());
    assert!(!from_file.ready.to_string().contains(&hex_psk));
    // Ready means a live production node: a peer holding the same PSK attaches
    // to the reported address, key and node id.
    attach(&from_file.ready, psk).expect("attach to the ready node");
    assert!(attach(&from_file.ready, [0x43; 32]).is_err());
    fx.json(&["down"]);
    drop(from_file);

    let from_stdin = fx.up_with_stdin(&["--psk-from", "stdin"], Some(&psk));
    assert_eq!(from_stdin.ready["psk_source"], "stdin");
    assert_eq!(from_stdin.ready["trust_domain"], expected.as_str());
    fx.json(&["down"]);
}

#[test]
fn unsupported_or_invalid_psk_sources_fail_before_starting() {
    let fx = Fx::new();
    let zero = fx.tmp.path().join("zero.hex");
    std::fs::write(&zero, "00".repeat(32)).unwrap();
    let short = fx.tmp.path().join("short.hex");
    std::fs::write(&short, "abcd").unwrap();
    #[cfg(unix)]
    for f in [&zero, &short] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let zero_src = format!("file:{}", zero.display());
    let short_src = format!("file:{}", short.display());
    for source in [
        "kms:aws:arn:placeholder",
        "4242424242424242424242424242424242424242424242424242424242424242",
        zero_src.as_str(),
        short_src.as_str(),
    ] {
        let out = fx.run(&["up", "--bind", "127.0.0.1:0", "--psk-from", source]);
        assert_eq!(out.status.code(), Some(2), "{source}: {out:?}");
        assert!(out.stdout.is_empty(), "{source}: printed {out:?}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("4242424242"),
            "{source}"
        );
    }
    // A refused source is rejected before any state is created.
    assert!(!fx.state().exists());
}

fn attach(ready: &Value, psk: [u8; 32]) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mesh = net_sdk::MeshBuilder::new("127.0.0.1:0", &psk)
            .unwrap()
            .build()
            .await
            .map_err(|e| e.to_string())?;
        mesh.start();
        let key: [u8; 32] = hex::decode(ready["public_key"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let node_id = u64::from_str_radix(
            ready["node_id"].as_str().unwrap().trim_start_matches("0x"),
            16,
        )
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            mesh.connect_via(ready["bind"].as_str().unwrap(), &key, node_id),
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

#[test]
fn a_port_squatter_holding_the_lock_is_never_reported_as_the_node() {
    let fx = Fx::new();
    let mut up = fx.up(&[]);
    let port = control_port(&fx.state());
    up.child.kill().unwrap();
    up.child.wait().unwrap();

    // Worst case for the client: something holds the lifetime lock AND answers
    // on the recorded control port, but does not know the per-run secret.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fx.state().join("node").join("up.lock"))
        .unwrap();
    lock.lock().unwrap();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let serve = std::thread::spawn(move || {
        for stream in squatter.incoming().take(2) {
            let mut s = stream.unwrap();
            let mut hello = b"NMCT".to_vec();
            hello.extend_from_slice(&[7u8; 32]);
            let _ = s.write_all(&hello);
            let mut proof = [0u8; 64];
            let _ = s.read_exact(&mut proof);
            // A forged "server proof": the squatter cannot compute the real one.
            let _ = s.write_all(&[9u8; 32]);
            // Read the client's (MAC'd) request, then send a well-formed frame
            // whose MAC is forged: the squatter lacks the session key.
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).is_ok() {
                let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                let _ = s.read_exact(&mut body);
            }
            let payload = br#"{"state":"ready","node":null,"accepted":true}"#;
            let _ = s.write_all(&((payload.len() + 32) as u32).to_be_bytes());
            let _ = s.write_all(payload);
            let _ = s.write_all(&[3u8; 32]);
        }
    });

    let status = fx.status();
    assert_eq!(status["state"], "unknown", "{status}");
    let down = fx.run(&["down"]);
    assert!(!down.status.success(), "down trusted an impostor: {down:?}");
    assert!(down.stdout.is_empty());
    serve.join().unwrap();
    drop(lock);
}

#[test]
fn the_control_endpoint_refuses_a_client_that_cannot_prove_the_secret() {
    let fx = Fx::new();
    let up = fx.up(&[]);
    let port = control_port(&fx.state());

    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut hello = [0u8; 36];
    s.read_exact(&mut hello).unwrap();
    assert_eq!(&hello[..4], b"NMCT");
    s.write_all(&[0x5A; 64]).unwrap();
    let mut rest = Vec::new();
    // The node closes without answering; nothing else is disclosed.
    let _ = s.read_to_end(&mut rest);
    assert!(rest.is_empty(), "node answered an unauthenticated client");

    // The node is unaffected and still stops through a real client.
    assert_eq!(fx.status()["state"], "ready");
    fx.json(&["down"]);
    drop(up);
}
