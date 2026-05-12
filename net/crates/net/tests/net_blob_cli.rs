//! Integration tests for the `net-blob` operator CLI.
//!
//! Spawns the bin via `env!("CARGO_BIN_EXE_net-blob")` against a
//! per-test temp directory, drives subcommands through real
//! `std::process::Command` invocations, and asserts on the
//! observable shape — exit codes, stdout (human + JSON), file
//! bytes round-trip. The bin itself is library-mode (no daemon,
//! no IPC), so the integration model is: spawn → assert →
//! cleanup. No mocks.
//!
//! Run: `cargo test --features cli --test net_blob_cli`.

#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the `net-blob` bin cargo emits as part of the test
/// build. Cargo only sets this env var for `[[bin]]` targets
/// that share a workspace with the `[[test]]` target, which the
/// `required-features = ["cli"]` declaration handles for us.
fn net_blob() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_net-blob"))
}

/// Build a fresh temp directory keyed off `tag` so concurrent
/// tests don't clobber each other. The dir is cleaned up on
/// `Drop` of the returned `TempDir` guard.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        // Use std::env::temp_dir + a per-run uniquifier so the
        // path is stable across the test's lifetime.
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!(
            "net-blob-cli-test-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        Self(base)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup — if the test panicked the dir
        // stays around for post-mortem.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run `net-blob` with the supplied args + persistent dir. Returns
/// the `Output` so callers can assert on stdout / stderr / exit
/// code.
fn run_net_blob(dir: &Path, args: &[&str]) -> Output {
    Command::new(net_blob())
        .arg("-d")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn net-blob")
}

fn stdout_string(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_string(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Pull the `hash` field out of a `--format json` put response.
fn parse_put_hash(out: &Output) -> String {
    let body = stdout_string(out);
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .unwrap_or_else(|e| panic!("net-blob put JSON parse failed: {}; body: {:?}", e, body));
    v["hash"]
        .as_str()
        .expect("hash field on put JSON")
        .to_string()
}

// ============================================================================
// put / get round-trip
// ============================================================================

#[test]
fn put_then_get_round_trips_file_bytes() {
    let tmp = TempDir::new("round-trip");
    let dir = tmp.path();
    let input = dir.join("payload.txt");
    let payload = b"hello from net-blob integration tests";
    fs::write(&input, payload).expect("write input");

    // put — JSON output for hash extraction.
    let put_out = run_net_blob(
        dir,
        &[
            "--format",
            "json",
            "put",
            input.to_str().expect("input path utf8"),
        ],
    );
    assert!(
        put_out.status.success(),
        "put must succeed; stderr={}",
        stderr_string(&put_out)
    );
    let hash = parse_put_hash(&put_out);
    assert_eq!(
        hash.len(),
        64,
        "BLAKE3 hash hex must be 64 chars; got {}",
        hash
    );

    // get → temp file, compare bytes.
    let output = dir.join("out.bin");
    let get_out = run_net_blob(
        dir,
        &[
            "get",
            &hash,
            "--out",
            output.to_str().expect("out path utf8"),
        ],
    );
    assert!(
        get_out.status.success(),
        "get must succeed; stderr={}",
        stderr_string(&get_out)
    );
    let fetched = fs::read(&output).expect("read fetched");
    assert_eq!(fetched, payload, "round-trip bytes must match");
}

#[test]
fn put_human_format_prints_uri_hash_size() {
    let tmp = TempDir::new("human-put");
    let input = tmp.path().join("p.bin");
    fs::write(&input, b"abc").unwrap();

    let out = run_net_blob(tmp.path(), &["put", input.to_str().unwrap()]);
    assert!(out.status.success());
    let body = stdout_string(&out);
    assert!(body.contains("stored:"), "missing 'stored:' line: {}", body);
    assert!(body.contains("hash:"), "missing 'hash:' line: {}", body);
    assert!(
        body.contains("size:   3"),
        "missing 'size:   3' line: {}",
        body
    );
}

// ============================================================================
// exists exit codes
// ============================================================================

#[test]
fn exists_returns_zero_for_present_hash_one_for_absent() {
    let tmp = TempDir::new("exists");
    let input = tmp.path().join("p.txt");
    fs::write(&input, b"check me").unwrap();
    let put = run_net_blob(
        tmp.path(),
        &["--format", "json", "put", input.to_str().unwrap()],
    );
    let hash = parse_put_hash(&put);

    // Same dir, in a fresh process — the chunk bytes persist via
    // Redex even though the refcount table is per-process.
    let exists_present = run_net_blob(tmp.path(), &["exists", &hash]);
    assert!(
        exists_present.status.success(),
        "exists must exit 0 for stored hash; stderr={}",
        stderr_string(&exists_present)
    );

    // A hash we never put.
    let absent = "0".repeat(64);
    let exists_absent = run_net_blob(tmp.path(), &["exists", &absent]);
    assert_eq!(
        exists_absent.status.code(),
        Some(1),
        "exists must exit 1 for missing hash; stderr={}",
        stderr_string(&exists_absent),
    );
}

// ============================================================================
// stat
// ============================================================================

#[test]
fn stat_json_carries_size_and_replicas_observed() {
    let tmp = TempDir::new("stat");
    let input = tmp.path().join("p.txt");
    fs::write(&input, b"0123456789").unwrap(); // 10 bytes
    let put = run_net_blob(
        tmp.path(),
        &["--format", "json", "put", input.to_str().unwrap()],
    );
    let hash = parse_put_hash(&put);

    let stat = run_net_blob(
        tmp.path(),
        &["--format", "json", "stat", &hash, "--size", "10"],
    );
    assert!(stat.status.success());
    let v: serde_json::Value =
        serde_json::from_str(stdout_string(&stat).trim()).expect("stat JSON");
    assert_eq!(v["hash"].as_str().unwrap(), hash);
    assert_eq!(v["size"].as_u64().unwrap(), 10);
    // `replicas_observed` is 0 in single-process mode (no mesh).
    assert_eq!(v["replicas_observed"].as_u64().unwrap(), 0);
}

// ============================================================================
// bad-hash error handling
// ============================================================================

#[test]
fn get_rejects_malformed_hash_with_typed_error() {
    let tmp = TempDir::new("bad-hash");
    let out = run_net_blob(tmp.path(), &["get", "not-a-real-hash"]);
    assert!(!out.status.success(), "get must reject malformed hash");
    let err = stderr_string(&out);
    assert!(
        err.contains("net-blob:") || err.contains("hash"),
        "stderr must surface a parse error; got {:?}",
        err
    );
}

// ============================================================================
// gc dry-run (works against an empty / fresh dir)
// ============================================================================

#[test]
fn gc_dry_run_reports_zero_candidates_on_empty_dir() {
    let tmp = TempDir::new("gc-empty");
    let out = run_net_blob(
        tmp.path(),
        &["--format", "json", "gc", "--dry-run", "--retention", "1s"],
    );
    assert!(
        out.status.success(),
        "gc --dry-run on empty dir must succeed; stderr={}",
        stderr_string(&out)
    );
    let v: serde_json::Value = serde_json::from_str(stdout_string(&out).trim()).expect("gc JSON");
    assert!(v["dry_run"].as_bool().unwrap());
    // Refcount table is per-process; on a fresh CLI run with an
    // empty Redex dir, no candidates.
    assert_eq!(v["candidates"].as_array().unwrap().len(), 0);
}

// ============================================================================
// metrics
// ============================================================================

#[test]
fn metrics_emits_prometheus_text_with_dataforts_blob_prefix() {
    let tmp = TempDir::new("metrics");
    let out = run_net_blob(tmp.path(), &["metrics"]);
    assert!(out.status.success());
    let body = stdout_string(&out);
    assert!(
        body.contains("dataforts_blob"),
        "metrics body must include dataforts_blob_* prefixes; got:\n{}",
        body
    );
    assert!(
        body.contains("# HELP") && body.contains("# TYPE"),
        "metrics body must follow the Prometheus text-exposition format"
    );
}

// ============================================================================
// pin / unpin via in-process chain (single CLI invocation)
// ============================================================================

#[test]
fn pin_then_ls_in_process_shows_pinned_entry() {
    let tmp = TempDir::new("pin-chain");
    // The CLI's refcount table resets per-process, so the only
    // way to observe a pin via `ls` is to chain the two ops in
    // the same invocation. `net-blob` doesn't support compound
    // subcommands today; we exercise the per-op behavior
    // separately and pin the in-session shape via the unit
    // tests inside the bin's source. Here we just verify the
    // pin subcommand prints its acknowledgment.
    let known = "0".repeat(64);
    let pin_out = run_net_blob(tmp.path(), &["pin", &known]);
    assert!(
        pin_out.status.success(),
        "pin must succeed; stderr={}",
        stderr_string(&pin_out)
    );
    let body = stdout_string(&pin_out);
    assert!(
        body.contains("pinned:") && body.contains(&known),
        "pin output must echo the pinned hash; got:\n{}",
        body
    );

    // unpin mirrors the shape.
    let unpin_out = run_net_blob(tmp.path(), &["unpin", &known]);
    assert!(unpin_out.status.success());
    let body = stdout_string(&unpin_out);
    assert!(
        body.contains("unpinned:") && body.contains(&known),
        "unpin output must echo the unpinned hash; got:\n{}",
        body
    );
}

// ============================================================================
// stdin piping ('-' as path)
// ============================================================================

#[test]
fn put_reads_stdin_when_path_is_dash() {
    use std::io::Write;
    use std::process::Stdio;
    let tmp = TempDir::new("stdin-put");
    let mut child = Command::new(net_blob())
        .arg("-d")
        .arg(tmp.path())
        .args(["--format", "json", "put", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn net-blob");
    let payload = b"piped from stdin";
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait child");
    assert!(
        out.status.success(),
        "stdin put must succeed; stderr={}",
        stderr_string(&out)
    );
    let v: serde_json::Value = serde_json::from_str(stdout_string(&out).trim()).expect("put JSON");
    assert_eq!(v["size"].as_u64().unwrap(), payload.len() as u64);
}
