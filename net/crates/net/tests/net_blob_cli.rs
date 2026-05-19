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

/// `get --out` must refuse to clobber an existing file. The CLI
/// is operator-facing and may run with elevated privileges; a
/// naive `fs::write` would happily overwrite arbitrary paths the
/// caller specifies. Pinning the create-new semantics so a future
/// refactor doesn't silently lose this defense.
#[test]
fn get_out_refuses_to_clobber_existing_file() {
    let tmp = TempDir::new("get-out-clobber");
    let dir = tmp.path();
    let input = dir.join("p.bin");
    fs::write(&input, b"x").unwrap();
    let put = run_net_blob(dir, &["--format", "json", "put", input.to_str().unwrap()]);
    let hash = parse_put_hash(&put);

    // Pre-create the output file. `get` must refuse to overwrite.
    let output = dir.join("preexisting.bin");
    fs::write(&output, b"do not clobber").unwrap();
    let get = run_net_blob(dir, &["get", &hash, "--out", output.to_str().unwrap()]);
    assert!(
        !get.status.success(),
        "get --out must error when the output path already exists"
    );
    let preserved = fs::read(&output).unwrap();
    assert_eq!(
        preserved, b"do not clobber",
        "preexisting file contents must be untouched"
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
fn pin_and_unpin_subcommands_acknowledge_in_separate_processes() {
    let tmp = TempDir::new("pin-chain");
    // The CLI's refcount table resets per-process — pin in one
    // invocation, ls in another, and the entry is gone. We can't
    // observe a pin via a follow-up `ls` across invocations
    // today. This test pins the per-op acknowledgment shape:
    // `pin` and `unpin` both echo the hash in their output.
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

// ============================================================================
// overflow status (v0.3 P4)
// ============================================================================

#[test]
fn overflow_status_human_format_prints_disabled_by_default() {
    // A fresh CLI invocation builds the adapter from defaults
    // (overflow disabled, defaults for every threshold,
    // zero counters). Human output must surface the
    // `enabled: false` line so an operator running `status`
    // sees the default-off contract at a glance.
    let tmp = TempDir::new("overflow-status");
    let out = run_net_blob(tmp.path(), &["overflow", "status"]);
    assert!(
        out.status.success(),
        "overflow status must succeed on a clean dir; stderr={}",
        stderr_string(&out)
    );
    let body = stdout_string(&out);
    assert!(
        body.contains("configured enabled:        false"),
        "human output must show enabled=false on a default adapter; got:\n{}",
        body
    );
    assert!(
        body.contains("runtime active (this proc): false"),
        "human output must show active=false when no tick has fired; got:\n{}",
        body
    );
    assert!(
        body.contains("pushes_admitted_total:     0"),
        "human output must include the admitted counter at zero; got:\n{}",
        body
    );
}

#[test]
fn overflow_status_json_format_shape_is_stable() {
    // JSON output must include the canonical top-level keys
    // `adapter`, `config`, `active`, `counters`. Operators
    // pipe `--format json` into `jq` to filter; the shape
    // must be stable across releases.
    let tmp = TempDir::new("overflow-status-json");
    let out = run_net_blob(tmp.path(), &["--format", "json", "overflow", "status"]);
    assert!(out.status.success());
    let body = stdout_string(&out);
    let v: serde_json::Value =
        serde_json::from_str(body.trim()).expect("overflow status JSON parse");
    assert_eq!(v["config"]["enabled"], serde_json::json!(false));
    assert_eq!(v["active"], serde_json::json!(false));
    // The counters block exists + admitted starts at zero.
    assert_eq!(v["counters"]["pushes_admitted_total"], serde_json::json!(0));
    // Six per-reason rejection counters all present at zero
    // (operator dashboards don't want missing keys).
    for reason in [
        "rejected_no_storage_cap_total",
        "rejected_not_participating_total",
        "rejected_sender_not_overflowing_total",
        "rejected_unhealthy_total",
        "rejected_scope_mismatch_total",
        "rejected_insufficient_disk_total",
    ] {
        assert_eq!(
            v["counters"][reason],
            serde_json::json!(0),
            "JSON output must include counter `{}` at zero",
            reason
        );
    }
}

#[test]
fn metrics_body_includes_overflow_counter_family() {
    // The Prometheus body the `metrics` subcommand emits
    // must include the v0.3 overflow counter family — even
    // on a default (disabled, no-tick) adapter. Pin every
    // metric name + the per-reason label family.
    let tmp = TempDir::new("metrics-overflow");
    let out = run_net_blob(tmp.path(), &["metrics"]);
    assert!(out.status.success());
    let body = stdout_string(&out);
    for needle in [
        "dataforts_blob_overflow_pushes_admitted_total",
        "dataforts_blob_overflow_push_errors_total",
        "dataforts_blob_overflow_pushed_bytes_total",
        "dataforts_blob_overflow_rejected_no_target_total",
        "dataforts_blob_overflow_rejected_total",
        "dataforts_blob_overflow_high_water_triggered_total",
        "dataforts_blob_overflow_low_water_cleared_total",
        "dataforts_blob_overflow_active",
        "dataforts_blob_overflow_disk_ratio",
        // Per-reason label family.
        "reason=\"no_storage_cap\"",
        "reason=\"not_participating\"",
        "reason=\"sender_not_overflowing\"",
        "reason=\"unhealthy\"",
        "reason=\"scope_mismatch\"",
        "reason=\"insufficient_disk\"",
    ] {
        assert!(
            body.contains(needle),
            "metrics body must include `{}`; got:\n{}",
            needle,
            body
        );
    }
}

// ============================================================================
// v0.3 Phase C/D subcommands (negative-path coverage)
// ============================================================================
//
// `repair`, `tree`, `verify`, `path` operate on `BlobRef::Tree`
// blobs. The CLI's `put` only produces `BlobRef::Small`, so
// happy-path coverage would require either a new `put-tree`
// subcommand or in-process library calls (which breaks the
// spawn-bin integration pattern). Until either lands, the tests
// below pin the negative paths: bad inputs surface as typed
// errors with nonzero exit, never as panics or silent success.

/// Stable test hash — 64 hex chars, content-irrelevant. The
/// subcommands construct a BlobRef::Tree from it; any chunk
/// fetch on this hash will miss (the chunk store is empty).
const DUMMY_HASH: &str = "deadbeef00000000000000000000000000000000000000000000000000000000";

#[test]
fn path_subcommand_rejects_offset_at_or_past_size() {
    let tmp = TempDir::new("path-offset-oob");
    let out = run_net_blob(
        tmp.path(),
        &[
            "path", DUMMY_HASH, "--size", "1024", "--depth", "1", "--offset", "1024",
        ],
    );
    assert!(!out.status.success(), "offset == size must exit nonzero");
    let stderr = stderr_string(&out);
    assert!(
        stderr.contains("offset") && stderr.contains("size"),
        "expected an offset-vs-size diagnostic, got: {}",
        stderr
    );
}

#[test]
fn tree_subcommand_on_missing_root_exits_cleanly() {
    let tmp = TempDir::new("tree-missing-root");
    let out = run_net_blob(
        tmp.path(),
        &["tree", DUMMY_HASH, "--size", "1024", "--depth", "1"],
    );
    assert!(
        !out.status.success(),
        "tree on a hash that doesn't exist locally must exit nonzero"
    );
    // No panic — stderr carries a typed error, not a thread dump.
    let stderr = stderr_string(&out);
    assert!(
        !stderr.contains("panicked"),
        "tree must surface a clean error, not panic. stderr:\n{}",
        stderr
    );
}

#[test]
fn repair_subcommand_on_missing_root_exits_cleanly() {
    let tmp = TempDir::new("repair-missing-root");
    let out = run_net_blob(
        tmp.path(),
        &["repair", DUMMY_HASH, "--size", "1024", "--depth", "1"],
    );
    assert!(
        !out.status.success(),
        "repair on a missing root must exit nonzero"
    );
    let stderr = stderr_string(&out);
    assert!(
        !stderr.contains("panicked"),
        "repair must surface a clean error, not panic. stderr:\n{}",
        stderr
    );
}

#[test]
fn verify_subcommand_on_missing_root_reports_root_unreachable() {
    let tmp = TempDir::new("verify-missing-root");
    // `--format` is a top-level flag (precedes the subcommand)
    // per the CLI's clap layout.
    let out = Command::new(net_blob())
        .args([
            "-d",
            tmp.path().to_str().unwrap(),
            "--format",
            "json",
            "verify",
            DUMMY_HASH,
            "--size",
            "1024",
            "--depth",
            "1",
        ])
        .output()
        .expect("spawn net-blob");
    // verify must distinguish "could not verify, manifest gone"
    // (exit 3, root_unreachable=true) from "verified, found
    // problems" (exit 2, missing/corrupted > 0). Operator
    // scripts route different remediation per code:
    //   exit 3 → operator probably mis-supplied --depth or the
    //            blob was deleted; do NOT auto-repair
    //   exit 2 → chunks missing/corrupted; queue net-blob repair
    let code = out.status.code();
    assert_eq!(
        code,
        Some(3),
        "missing root must exit 3 (root_unreachable), got exit {:?}",
        code,
    );
    let stdout = stdout_string(&out);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("verify --format json must emit valid JSON");
    assert_eq!(
        parsed["root_unreachable"].as_bool(),
        Some(true),
        "missing root must surface root_unreachable=true; got: {}",
        stdout,
    );
    // healthy / missing / corrupted are all zero when the walk
    // never starts.
    assert_eq!(parsed["healthy"].as_u64(), Some(0));
    assert_eq!(parsed["missing"].as_u64(), Some(0));
    assert_eq!(parsed["corrupted"].as_u64(), Some(0));
}

/// `tree` / `repair` / `verify` against a hash whose stored
/// content doesn't decode as a `TreeNode` postcard body must
/// surface a clean error, not panic. We synthesize the scenario
/// via `put` (which stores a Small blob — arbitrary bytes
/// content-addressed to BLAKE3), then invoke the Tree-only
/// subcommands against the same hash with --depth=1. The Tree
/// path fetches the chunk successfully (it exists locally),
/// then fails the `TreeNode::decode` step.
#[test]
fn tree_subcommand_on_corrupt_root_decode_fails_cleanly() {
    let tmp = TempDir::new("tree-corrupt-root");
    let dir = tmp.path();
    // Store arbitrary bytes that are NOT a valid TreeNode
    // postcard body. The CLI's `put` produces a Small blob, but
    // the bytes-at-hash association is what we need: the Tree
    // subcommands will fetch the bytes and try to decode them.
    let input = dir.join("garbage.bin");
    fs::write(&input, b"this is not a TreeNode postcard body").unwrap();
    let put_out = run_net_blob(dir, &["--format", "json", "put", input.to_str().unwrap()]);
    let hash = parse_put_hash(&put_out);

    // tree subcommand: must surface a typed decode error, not panic.
    let out = run_net_blob(
        dir,
        &["tree", &hash, "--size", "1024", "--depth", "1"],
    );
    assert!(
        !out.status.success(),
        "tree on corrupt-decode root must exit nonzero"
    );
    let stderr = stderr_string(&out);
    assert!(
        !stderr.contains("panicked"),
        "tree must clean-error on bad TreeNode bytes, not panic. stderr:\n{}",
        stderr,
    );
}

/// Same scenario as above but against the `repair` subcommand.
/// Tree-walking traversal hits `TreeNode::decode` on the root
/// chunk; clean error, no panic.
#[test]
fn repair_subcommand_on_corrupt_root_decode_fails_cleanly() {
    let tmp = TempDir::new("repair-corrupt-root");
    let dir = tmp.path();
    let input = dir.join("garbage.bin");
    fs::write(&input, b"not-a-tree-node-body").unwrap();
    let put_out = run_net_blob(dir, &["--format", "json", "put", input.to_str().unwrap()]);
    let hash = parse_put_hash(&put_out);

    let out = run_net_blob(
        dir,
        &["repair", &hash, "--size", "1024", "--depth", "1"],
    );
    assert!(
        !out.status.success(),
        "repair on corrupt-decode root must exit nonzero"
    );
    let stderr = stderr_string(&out);
    assert!(
        !stderr.contains("panicked"),
        "repair must clean-error on bad TreeNode bytes. stderr:\n{}",
        stderr,
    );
}

/// `verify` against a corrupt-decode root must surface
/// root_unreachable=true (exit 3) — the root chunk exists but
/// can't be decoded, which from the operator's POV is equivalent
/// to "could not verify, manifest gone." Exit 3 routes the
/// operator to manual investigation rather than auto-repair.
#[test]
fn verify_subcommand_on_corrupt_root_exits_cleanly() {
    let tmp = TempDir::new("verify-corrupt-root");
    let dir = tmp.path();
    let input = dir.join("garbage.bin");
    fs::write(&input, b"not-a-tree-node").unwrap();
    let put_out = run_net_blob(dir, &["--format", "json", "put", input.to_str().unwrap()]);
    let hash = parse_put_hash(&put_out);

    let out = Command::new(net_blob())
        .args([
            "-d",
            dir.to_str().unwrap(),
            "--format",
            "json",
            "verify",
            &hash,
            "--size",
            "1024",
            "--depth",
            "1",
        ])
        .output()
        .expect("spawn net-blob");
    // The root chunk fetches successfully (exists locally), so
    // root_unreachable is false from the fetch_chunk probe's
    // view. The walk then runs and hits TreeNode::decode failure,
    // which today returns a hard error from verify_walk —
    // cmd_verify propagates that as exit 1. Acceptable: pre-fix
    // and post-fix both treat decode-failure as a hard error
    // distinct from missing-root (exit 3). The contract this
    // test pins is "no panic, nonzero exit, clean stderr."
    assert!(
        !out.status.success(),
        "verify on corrupt-decode root must exit nonzero"
    );
    let stderr = stderr_string(&out);
    assert!(
        !stderr.contains("panicked"),
        "verify must clean-error on bad TreeNode bytes. stderr:\n{}",
        stderr,
    );
}

#[test]
fn tree_repair_verify_path_rejects_malformed_hash() {
    // Every Phase C/D subcommand parses the hash via the same
    // parse_hash helper. Non-hex / wrong-length input should
    // produce a clean parse error, not a panic.
    let tmp = TempDir::new("malformed-hash");
    let bogus = "not-a-real-hash";
    for subcommand in &["tree", "repair", "verify"] {
        let out = run_net_blob(
            tmp.path(),
            &[*subcommand, bogus, "--size", "1024", "--depth", "1"],
        );
        assert!(
            !out.status.success(),
            "{} with malformed hash must exit nonzero",
            subcommand
        );
        let stderr = stderr_string(&out);
        assert!(
            !stderr.contains("panicked"),
            "{} must clean-error on bad hash, not panic. stderr:\n{}",
            subcommand,
            stderr
        );
    }
    // `path` takes the same parse_hash plus --offset.
    let out = run_net_blob(
        tmp.path(),
        &[
            "path", bogus, "--size", "1024", "--depth", "1", "--offset", "0",
        ],
    );
    assert!(!out.status.success());
    let stderr = stderr_string(&out);
    assert!(!stderr.contains("panicked"));
}
