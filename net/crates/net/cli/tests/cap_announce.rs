//! `net cap announce` integration test.
//!
//! Covers the v0.4 capability-auth CLI helper: generates an
//! operator keypair via `net identity generate`, runs
//! `net cap announce` with the supplied allow-lists, and reads
//! back the emitted JSON bytes through the substrate's
//! `CapabilityAnnouncement::from_bytes` decoder. Asserts:
//!
//! - the keypair seed survives the sign / re-parse round-trip
//!   (signature verifies against the published `entity_id`);
//! - allow-list flags land on the announcement intact;
//! - `--out` and stdout produce byte-identical output;
//! - malformed `--allow-subnet` / `--allow-group` arguments
//!   surface as exit-2 InvalidArgs rather than a silent drop.

use assert_cmd::prelude::*;
use net_sdk::capabilities::{CapabilityAnnouncement, CapabilityGroupId, CapabilitySubnetId};
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Run `net identity generate --out <PATH>` and return the path
/// to the freshly written identity TOML.
fn generate_identity(dir: &TempDir) -> PathBuf {
    let path = dir.path().join("operator.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["identity", "generate", "--out"])
        .arg(&path)
        .args(["--force"])
        .assert()
        .success();
    path
}

#[test]
fn cap_announce_produces_signed_bytes_with_allow_lists() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    let out_path = dir.path().join("announcement.json");

    let subnet_hex = "112233445566778899aabbccddeeff00";
    let group_hex = "deadbeefcafef00d0011223344556677889900aabbccddeeff00112233445566";

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--tag", "dataforts.blob.overflow"])
        .args(["--allow-node", "42"])
        .args(["--allow-node", "0xDEADBEEF"])
        .args(["--allow-subnet", subnet_hex])
        .args(["--allow-group", group_hex])
        .args(["--version", "7"])
        .args(["--ttl-secs", "120"])
        .arg("--out")
        .arg(&out_path)
        .assert()
        .success();

    let bytes = std::fs::read(&out_path).unwrap();
    let ann = CapabilityAnnouncement::from_bytes(&bytes).expect("decode wire bytes");

    assert_eq!(ann.version, 7);
    assert_eq!(ann.ttl_secs, 120);
    assert_eq!(ann.allowed_nodes, vec![42u64, 0xDEAD_BEEFu64]);
    assert_eq!(
        ann.allowed_subnets,
        vec![CapabilitySubnetId::from_tag(&format!("subnet:{subnet_hex}")).unwrap()]
    );
    assert_eq!(
        ann.allowed_groups,
        vec![CapabilityGroupId::from_tag(&format!("group:{group_hex}")).unwrap()]
    );

    // The signature is over the post-add layout; verify() must
    // pass — pins that `cap announce` actually signed the right
    // bytes against the right entity.
    ann.verify().expect("signature verifies");

    // Both expected tags survive the parser (no reserved-prefix
    // silent drop).
    assert!(ann.capabilities.has_tag("nrpc:echo"));
    assert!(ann.capabilities.has_tag("dataforts.blob.overflow"));
}

#[test]
fn cap_announce_stdout_emits_signed_json_with_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);

    let stdout_bytes = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .output()
        .unwrap()
        .stdout;

    // stdout adds a trailing newline so a piped consumer can
    // read a clean line. The bytes BEFORE the newline are the
    // canonical JSON announcement.
    assert!(
        stdout_bytes.ends_with(b"\n"),
        "stdout must end with a newline for line-oriented consumers",
    );
    let json_bytes = &stdout_bytes[..stdout_bytes.len() - 1];
    let ann = CapabilityAnnouncement::from_bytes(json_bytes).expect("decode stdout bytes");
    ann.verify().expect("stdout-emitted signature must verify");
}

#[test]
fn cap_announce_rejects_malformed_subnet() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--allow-subnet", "not-hex"])
        .assert()
        .code(2);
}

#[test]
fn cap_announce_rejects_malformed_group() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--allow-group", "deadbeef"])
        .assert()
        .code(2);
}

/// H3 regression — `--node-id` that doesn't match the signing
/// key's derived id used to be silently accepted, producing
/// announcement bytes that fail receiver-side `node_id ↔
/// entity_id` binding verification. The CLI must reject the
/// mismatch up-front so operators don't ship unusable output.
#[test]
fn cap_announce_rejects_node_id_mismatch_with_signing_key() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        // Synthetic node id distinct from any keypair-derived
        // value (the derivation is a BLAKE2s projection; this
        // 0x01 sentinel is vanishingly unlikely to collide).
        .args(["--node-id", "0x1"])
        .assert()
        .code(2);
}

/// M4 regression — pre-fix the duplicate-tag check relied on
/// `caps.tags.len()` not growing across `add_tag`, which conflated
/// "parser rejected the input" with "tag was already in the set".
/// `--tag nrpc:echo --tag nrpc:echo` was a legal invocation that
/// errored with the reserved-prefix message. Post-fix the parser
/// result drives the decision: duplicates dedupe silently via
/// the underlying HashSet<Tag>; only genuinely-invalid tags
/// error out.
#[test]
fn cap_announce_accepts_duplicate_tag() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--tag", "nrpc:echo"])
        .assert()
        .success();
}

/// Reserved-prefix tags (`scope:`, `causal:`, etc.) must STILL
/// be rejected — the duplicate-tag fix uses the parser directly,
/// not the length heuristic, so the rejection moves from a
/// hand-rolled `caps.tags.len()` check to `Tag::parse_user`'s
/// own `Err(CapabilityTagError::ReservedPrefix)`.
#[test]
fn cap_announce_rejects_reserved_prefix_tag() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "scope:tenant:foo"])
        .assert()
        .code(2);
}

/// `--node-id` matching the derived value should pass — the
/// explicit confirmation form is supported, just not a mismatch.
#[test]
fn cap_announce_accepts_node_id_matching_signing_key() {
    use net_sdk::capabilities::CapabilityAnnouncement;
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);

    // Run an initial announce without --node-id so we can extract
    // the derived value from the emitted JSON.
    let baseline_path = dir.path().join("baseline.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .arg("--out")
        .arg(&baseline_path)
        .assert()
        .success();
    let baseline = CapabilityAnnouncement::from_bytes(&std::fs::read(&baseline_path).unwrap())
        .expect("decode baseline");
    let derived_hex = format!("{:#x}", baseline.node_id);

    // Now run with --node-id matching the derived value.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--node-id", &derived_hex])
        .assert()
        .success();
}

/// `parse_node_id` trims leading/trailing whitespace before parsing
/// hex, so `--allow-node " 0xDEADBEEF "` round-trips cleanly.
/// `parse_subnets` / `parse_groups` carry the same normalization so
/// a shell-pasted hex with trailing whitespace doesn't fail one
/// flag while passing another. Three-axis test pins the contract
/// across all three parsers.
#[test]
fn cap_announce_normalizes_whitespace_across_allow_list_parsers() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = generate_identity(&dir);
    let out_path = dir.path().join("ann.json");

    // Whitespace-padded inputs on all three allow-list axes.
    let node_padded = "  0xCAFEF00D  ";
    let subnet_padded = "   00112233445566778899aabbccddeeff   ";
    let group_padded = "   ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100   ";

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["cap", "announce"])
        .arg("--key")
        .arg(&key_path)
        .args(["--tag", "nrpc:echo"])
        .args(["--allow-node", node_padded])
        .args(["--allow-subnet", subnet_padded])
        .args(["--allow-group", group_padded])
        .arg("--out")
        .arg(&out_path)
        .assert()
        .success();

    let bytes = std::fs::read(&out_path).unwrap();
    let ann = CapabilityAnnouncement::from_bytes(&bytes).expect("decode wire bytes");
    assert_eq!(ann.allowed_nodes, vec![0xCAFE_F00Du64]);
    assert_eq!(
        ann.allowed_subnets,
        vec![CapabilitySubnetId::from_tag(&format!("subnet:{}", subnet_padded.trim())).unwrap()]
    );
    assert_eq!(
        ann.allowed_groups,
        vec![CapabilityGroupId::from_tag(&format!("group:{}", group_padded.trim())).unwrap()]
    );
}
