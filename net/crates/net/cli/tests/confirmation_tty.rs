// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real native PTY confirmation. Pipes/booleans cannot prove terminal behavior.
use std::io::{Read, Write};
use std::time::{Duration, Instant};

struct ChildGuard(Box<dyn portable_pty::Child + Send + Sync>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn exercise(flag: Option<&str>, answer: Option<&str>, expected: u32) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let identity = dir.path().join("operator.toml");
    assert_cmd::Command::cargo_bin("net-mesh")
        .unwrap()
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .arg("--config")
        .arg(&config)
        .args(["identity", "generate", "--out"])
        .arg(&identity)
        .assert()
        .success();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 60,
            cols: 240,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = portable_pty::CommandBuilder::new(assert_cmd::cargo::cargo_bin("net-mesh"));
    cmd.env_remove("NET_MESH_CONFIG");
    cmd.env_remove("NET_MESH_PROFILE");
    cmd.env("NO_COLOR", "1");
    cmd.arg("--config");
    cmd.arg(&config);
    cmd.args([
        "--output",
        "json",
        "ice",
        "freeze-cluster",
        "--local",
        "--ttl",
        "1m",
        "--identity",
    ]);
    cmd.arg(&identity);
    if let Some(flag) = flag {
        cmd.arg(flag);
    }
    let mut child = ChildGuard(pair.slave.spawn_command(cmd).unwrap());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 || tx.send(buffer[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    let mut transcript = String::new();
    let mut answered = false;
    let mut cursor_answered = false;
    let code = loop {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "terminal child hung: {transcript}"
        );
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(20)) {
            transcript.push_str(&String::from_utf8_lossy(&bytes));
        }
        if !cursor_answered && transcript.contains("\x1b[6n") {
            writer.write_all(b"\x1b[1;1R").unwrap();
            writer.flush().unwrap();
            cursor_answered = true;
        }
        if !answered && transcript.contains("Type YES to confirm ICE commit:") {
            assert!(
                answer.is_some(),
                "unexpected confirmation prompt: {transcript}"
            );
            if let Some(answer) = answer {
                writer.write_all(format!("{answer}\r").as_bytes()).unwrap();
                writer.flush().unwrap();
                answered = true;
            }
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            break status.exit_code();
        }
    };
    drop(writer);
    drop(pair.master);
    reader_thread.join().unwrap();
    for bytes in rx {
        transcript.push_str(&String::from_utf8_lossy(&bytes));
    }
    assert_eq!(code, expected, "{transcript}");
    assert!(!transcript.contains("stdin is not a TTY"), "{transcript}");
    assert_eq!(
        transcript.contains("Type YES to confirm ICE commit:"),
        answer.is_some(),
        "{transcript}"
    );
    assert_eq!(answered, answer.is_some());
    assert_eq!(
        transcript.contains("\"commit_id\""),
        expected == 0 && flag != Some("--dry-run"),
        "{transcript}"
    );
    // PTYs merge terminal output; separate stdout framing is covered by the
    // pipe-driven automation_contract witnesses, not inferred here.
}

#[test]
fn terminal_accepts_yes() {
    exercise(None, Some("YES"), 0);
}
#[test]
fn terminal_refuses_no() {
    exercise(None, Some("NO"), 8);
}
#[test]
fn terminal_yes_flag_never_prompts() {
    exercise(Some("--yes"), None, 0);
}
#[test]
fn terminal_dry_run_never_prompts() {
    exercise(Some("--dry-run"), None, 0);
}
