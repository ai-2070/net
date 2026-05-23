//! Pin the `--print-bootstrap` JSON line shape.
//!
//! Stage 6 of `SDK_AGGREGATOR_SUBNET_PLAN.md` — every binding
//! integration test (Node / Python / Go) parses this exact
//! line shape to drive its handshake against a daemon
//! subprocess. The shape is locked: `{"node_id":N,
//! "bound_addr":"IP:PORT","public_key_hex":"<64 hex>"}`.
//!
//! This test spawns the actual binary, captures stdout's first
//! line, and asserts the three fields. Catches any drift
//! introduced by maintainers tweaking the print format.

use std::io::BufRead;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Build path to the daemon binary. `cargo test` arranges
/// `CARGO_BIN_EXE_<name>` env vars for binaries in the same
/// crate.
fn daemon_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_net-aggregator-daemon"))
}

#[test]
fn print_bootstrap_emits_one_json_line_with_locked_fields() {
    // Minimum-viable config: ephemeral port, no groups, no
    // templates — keeps the daemon's startup fast.
    let mut cfg = tempfile::NamedTempFile::new().expect("tempfile");
    writeln!(
        cfg,
        r#"
listen = "127.0.0.1:0"
psk_hex = "4242424242424242424242424242424242424242424242424242424242424242"
"#
    )
    .expect("write cfg");
    cfg.flush().expect("flush cfg");

    let mut child = Command::new(daemon_bin())
        .arg("--config")
        .arg(cfg.path())
        .arg("--print-bootstrap")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let stdout = child.stdout.take().expect("daemon stdout pipe");
    let mut reader = std::io::BufReader::new(stdout);
    let mut line = String::new();

    // Wait up to 5 s for the line to land. Polling because the
    // daemon emits the line synchronously between mesh.start()
    // and wait_for_shutdown — typically <50 ms.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_line = false;
    while Instant::now() < deadline {
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => {
                got_line = true;
                break;
            }
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    // Kill the daemon before asserting so a failure doesn't
    // leave a stranded process.
    let _ = child.kill();
    let _ = child.wait();

    assert!(got_line, "daemon never emitted the bootstrap line");
    let trimmed = line.trim_end();
    // Field-level checks — substring assertions tolerate
    // numeric / string-value differences across runs while
    // pinning the field names + JSON shape.
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "expected single-line JSON object, got: {trimmed}"
    );
    assert!(
        trimmed.contains("\"node_id\":"),
        "missing node_id field: {trimmed}"
    );
    assert!(
        trimmed.contains("\"bound_addr\":\"127.0.0.1:"),
        "missing/wrong bound_addr field: {trimmed}"
    );
    assert!(
        trimmed.contains("\"public_key_hex\":\""),
        "missing public_key_hex field: {trimmed}"
    );
    // Public key must be exactly 64 hex chars between the
    // quoted boundaries.
    let pk_start = trimmed.find("\"public_key_hex\":\"").unwrap() + "\"public_key_hex\":\"".len();
    let pk_end = trimmed[pk_start..].find('"').unwrap() + pk_start;
    let pk = &trimmed[pk_start..pk_end];
    assert_eq!(pk.len(), 64, "public_key_hex must be 64 chars, got {pk:?}");
    assert!(
        pk.chars()
            .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())),
        "public_key_hex must be lowercase hex, got {pk:?}"
    );
}
