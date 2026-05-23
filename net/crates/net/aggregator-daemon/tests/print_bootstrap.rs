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

    // Parse — `from_str::<Value>` validates the JSON shape end
    // to end (catches trailing commas, escape-mismatch bugs,
    // field-order regressions that substring-asserts miss).
    let parsed: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("bootstrap line is not valid JSON ({e}): {trimmed}"));
    let obj = parsed
        .as_object()
        .unwrap_or_else(|| panic!("bootstrap line is not a JSON object: {trimmed}"));

    assert!(
        obj.get("node_id").and_then(|v| v.as_u64()).is_some(),
        "missing/wrong node_id field: {trimmed}",
    );
    let bound_addr = obj
        .get("bound_addr")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing bound_addr string field: {trimmed}"));
    assert!(
        bound_addr.starts_with("127.0.0.1:"),
        "bound_addr should reflect the loopback bind, got {bound_addr:?}",
    );
    let pk = obj
        .get("public_key_hex")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing public_key_hex string field: {trimmed}"));
    assert_eq!(pk.len(), 64, "public_key_hex must be 64 chars, got {pk:?}");
    assert!(
        pk.chars()
            .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())),
        "public_key_hex must be lowercase hex, got {pk:?}",
    );
}
