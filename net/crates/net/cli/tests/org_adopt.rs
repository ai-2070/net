//! OA-1 CLI flow — `net org keygen` → `issue-cert` →
//! `issue-floors` → `net node adopt` (ORG_CAPABILITY_AUTH_PLAN.md
//! §1.1–1.5).
//!
//! Drives the real binary end-to-end against a tempdir: key
//! material lands 0600, adoption provisions the three authority
//! files, one-node-one-owner refuses a second org loudly, and a
//! floors bundle merges monotonically during adoption.

use assert_cmd::prelude::*;
use std::path::Path;
use std::process::Command;

fn keygen(dir: &Path, name: &str) -> std::path::PathBuf {
    let key = dir.join(name);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(0);
    key
}

fn issue_cert(
    dir: &Path,
    key: &Path,
    member_hex: &str,
    generation: u32,
    name: &str,
) -> std::path::PathBuf {
    let cert = dir.join(name);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(key)
        .args(["--member", member_hex])
        .args(["--generation", &generation.to_string()])
        .args(["--out"])
        .arg(&cert)
        .assert()
        .code(0);
    cert
}

/// A stable fake node entity id (any 32 bytes decode as an
/// EntityId; adoption only needs the public half).
const MEMBER_HEX: &str = "2424242424242424242424242424242424242424242424242424242424242424";

#[test]
fn keygen_writes_owner_only_key_and_refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "org key must be owner-only, got {mode:o}");
    }

    // Refuses to clobber without --force.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(2);
}

#[test]
fn full_adopt_flow_provisions_authority_files() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 0, "node.cert.json");

    let authority = dir.path().join("authority");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .assert()
        .code(0);

    for file in [
        "owner-membership.json",
        "owner-audience.key",
        "revocation-state.json",
    ] {
        assert!(
            authority.join(file).exists(),
            "{file} must exist after adopt"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(authority.join("owner-audience.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "owner-audience.key must be owner-only, got {mode:o}"
        );
    }
}

#[test]
fn adopt_refuses_wrong_entity_and_second_owner() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 0, "node.cert.json");
    let authority = dir.path().join("authority");

    // Cert names MEMBER_HEX; adopting as a different entity is a
    // loud refusal, and nothing is installed.
    let other = "9999999999999999999999999999999999999999999999999999999999999999";
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", other])
        .args(["--authority-dir"])
        .arg(&authority)
        .assert()
        .code(3);
    assert!(!authority.join("owner-membership.json").exists());

    // Proper adoption, then a SECOND org's cert for the same node:
    // one node one owner.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .assert()
        .code(0);

    let key_b = keygen(dir.path(), "org-b.toml");
    let cert_b = issue_cert(dir.path(), &key_b, MEMBER_HEX, 0, "node.cert-b.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert_b)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .assert()
        .code(3);
}

#[test]
fn issue_floors_and_adopt_applies_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    // Cert at generation 5 so it survives the floor below.
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 5, "node.cert.json");

    let floors = dir.path().join("floors.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-floors", "--org-key"])
        .arg(&key)
        .args(["--floor", &format!("{MEMBER_HEX}=5")])
        .args(["--out"])
        .arg(&floors)
        .assert()
        .code(0);

    let authority = dir.path().join("authority");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .args(["--floors"])
        .arg(&floors)
        .assert()
        .code(0);

    // The persisted maxima carry the floor.
    let state = std::fs::read_to_string(authority.join("revocation-state.json")).unwrap();
    assert!(state.contains("\"floor\": 5"), "state: {state}");

    // A generation-0 cert (below the now-persisted floor 5) is
    // refused at re-adoption — the floor outlives the ceremony
    // that installed it.
    let low_cert = issue_cert(dir.path(), &key, MEMBER_HEX, 0, "node.cert-low.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&low_cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .assert()
        .code(3);
}

/// Review-8 §7 real-CLI red, reproduced as a witness: a certificate
/// the supplied floor bundle immediately revokes must never adopt —
/// nonzero exit, nothing provisioned.
#[test]
fn adopt_refuses_cert_below_supplied_floor() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 3, "node.cert.json");

    let floors = dir.path().join("floors.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-floors", "--org-key"])
        .arg(&key)
        .args(["--floor", &format!("{MEMBER_HEX}=5")])
        .args(["--out"])
        .arg(&floors)
        .assert()
        .code(0);

    let authority = dir.path().join("authority");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .args(["--floors"])
        .arg(&floors)
        .assert()
        .code(3);
    // Nothing was provisioned by the refused ceremony.
    for file in [
        "owner-membership.json",
        "owner-audience.key",
        "revocation-state.json",
    ] {
        assert!(
            !authority.join(file).exists(),
            "{file} must not exist after a refused adoption"
        );
    }
}

/// Review-8 §6: a bundle signed by a foreign org is refused before
/// durable state changes — no foreign floors are ever persisted
/// through the owner-adoption ceremony.
#[test]
fn adopt_refuses_foreign_floor_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let key_a = keygen(dir.path(), "org-a.toml");
    let key_b = keygen(dir.path(), "org-b.toml");
    let cert_a = issue_cert(dir.path(), &key_a, MEMBER_HEX, 0, "node.cert.json");

    // B signs a perfectly valid bundle for the same member.
    let floors_b = dir.path().join("floors-b.json");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-floors", "--org-key"])
        .arg(&key_b)
        .args(["--floor", &format!("{MEMBER_HEX}=5")])
        .args(["--out"])
        .arg(&floors_b)
        .assert()
        .code(0);

    let authority = dir.path().join("authority");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert_a)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .args(["--floors"])
        .arg(&floors_b)
        .assert()
        .code(3);
    assert!(
        !authority.join("revocation-state.json").exists(),
        "no B floor may be persisted"
    );
}

/// Review-8 §11: a skew above the token ceiling is rejected as
/// invalid arguments (exit 2) before anything is written.
#[test]
fn adopt_rejects_over_ceiling_skew_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 0, "node.cert.json");

    let authority = dir.path().join("authority");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["node", "adopt", "--cert"])
        .arg(&cert)
        .args(["--entity", MEMBER_HEX])
        .args(["--authority-dir"])
        .arg(&authority)
        .args(["--skew-secs", "301"])
        .assert()
        .code(2);
    assert!(!authority.exists() || !authority.join("owner-membership.json").exists());
}

#[test]
fn issue_cert_rejects_overlong_ttl_and_bad_member() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");

    // TTL past the 2-year ceiling: refused at issue.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--ttl-secs", "999999999"])
        .args(["--out"])
        .arg(dir.path().join("never.json"))
        .assert()
        .code(2);

    // Malformed member hex.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", "zznothex"])
        .args(["--out"])
        .arg(dir.path().join("never.json"))
        .assert()
        .code(2);
}

// =========================================================================
// §2 — `--force` must never destroy the org root key.
//
// `run_issue_cert` / `run_issue_floors` used to call `refuse_existing`
// (which returns Ok on the first line when `--force` is set) and then
// `tokio::fs::write`, which truncates in place and writes THROUGH a leaf
// symlink. Pointing `--out` at `--org-key` with `--force` therefore replaced
// the org root seed with cert JSON: no node can be re-certified, no
// revocation floor can ever be issued again, and every outstanding
// membership cert stays valid until natural expiry with no way to revoke it.
//
// Each test below asserts the org key SURVIVED BYTE-FOR-BYTE, not merely
// that the command exited non-zero — an exit-code-only assertion passes for
// any of several unrelated reasons (clap's usage code is 2, and so is
// `ExitCodeKind::InvalidArgs`).
// =========================================================================

/// The org key file's exact bytes, for before/after comparison.
fn read_key_bytes(key: &Path) -> Vec<u8> {
    std::fs::read(key).expect("org key readable")
}

#[test]
fn issue_cert_force_refuses_to_alias_the_org_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let before = read_key_bytes(&key);

    let assert = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--out"])
        .arg(&key) // <-- the org key itself
        .arg("--force")
        .assert()
        .code(2);

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("--org-key") && stderr.contains("--out"),
        "the refusal must name the two aliased flags so the operator can fix \
         the invocation; got: {stderr}",
    );
    assert_eq!(
        read_key_bytes(&key),
        before,
        "the org root key must be byte-for-byte intact after a refused --force",
    );
}

#[test]
fn issue_floors_force_refuses_to_alias_the_org_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let before = read_key_bytes(&key);

    let assert = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-floors", "--org-key"])
        .arg(&key)
        .args(["--floor", &format!("{MEMBER_HEX}=3")])
        .args(["--out"])
        .arg(&key)
        .arg("--force")
        .assert()
        .code(2);

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("--org-key") && stderr.contains("--out"),
        "the refusal must name the two aliased flags; got: {stderr}",
    );
    assert_eq!(
        read_key_bytes(&key),
        before,
        "the org root key must be byte-for-byte intact after a refused --force",
    );
}

/// The lexical alias guard only compares the two paths it was given. It
/// cannot see that a THIRD path holds key material — a backup copy of the org
/// key, a second checkout, a key restored beside the artifact it signs. With
/// `--force` that destination would be replaced.
///
/// `refuse_replacing_org_key` is the backstop: it refuses on the
/// destination's CONTENT, so the spelling is irrelevant. Exercised here with
/// a copy at a lexically unrelated path — which the alias guard provably
/// cannot catch, so a pass witnesses the content check specifically.
#[test]
fn issue_cert_force_refuses_a_destination_holding_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");

    // A backup copy at an unrelated path — `refuse_aliased_paths` compares
    // `--org-key` against `--out` lexically and sees two different files.
    let backup = dir.path().join("org-key-backup.toml");
    std::fs::copy(&key, &backup).unwrap();
    let before = read_key_bytes(&backup);

    let assert = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--out"])
        .arg(&backup)
        .arg("--force")
        .assert()
        .code(2);

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("org root key material"),
        "the content backstop (not the lexical guard) must be what refuses \
         this destination; got: {stderr}",
    );
    assert!(
        !stderr.contains("seed_hex"),
        "the refusal must not echo the key file's contents: {stderr}",
    );
    assert_eq!(
        read_key_bytes(&backup),
        before,
        "key material at the destination must be byte-for-byte intact",
    );
}

/// Positive control: `--force` still does its job. Certificates are renewable
/// by design (~annual), so refusing `--force` outright the way the grant verbs
/// do would break a real workflow — the fix makes the replace SAFE (staged +
/// atomic rename), not unavailable.
#[test]
fn issue_cert_force_still_renews_an_existing_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let cert = issue_cert(dir.path(), &key, MEMBER_HEX, 0, "node.cert.json");
    let first = std::fs::read(&cert).unwrap();

    // Re-issue at a higher generation over the same path.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--generation", "7"])
        .args(["--out"])
        .arg(&cert)
        .arg("--force")
        .assert()
        .code(0);

    let second = std::fs::read(&cert).unwrap();
    assert_ne!(first, second, "--force must actually replace the artifact");

    // The published artifact must be COMPLETE, not a torn/interleaved write —
    // the whole point of staging plus an atomic rename. It parses as the
    // versioned envelope, and the cert body carries the new generation: the
    // canonical cert encoding is fixed-offset with `generation` as a
    // little-endian u32, so generation 7 appears as `07000000` in the hex
    // body (a gen-0 cert has `00000000` there).
    let text = String::from_utf8_lossy(&second).to_string();
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("published cert is valid JSON");
    assert_eq!(parsed["version"], 1, "versioned envelope preserved");
    let body = parsed["cert"].as_str().expect("cert body is a hex string");
    assert!(
        body.contains("07000000"),
        "the replacement must carry the newly issued generation (LE u32 in the \
         canonical cert encoding); body: {body}",
    );
    // No staging temp left behind.
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".stage."))
        .collect();
    assert!(strays.is_empty(), "staging temps left behind: {strays:?}");
}

/// Without `--force`, publication is no-clobber and must not write THROUGH a
/// leaf symlink onto the org key. `tokio::fs::write` followed symlinks; the
/// staged hard-link publish does not.
#[cfg(unix)]
#[test]
fn issue_cert_never_writes_through_a_symlink_onto_the_org_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let before = read_key_bytes(&key);

    let link = dir.path().join("cert.json");
    std::os::unix::fs::symlink(&key, &link).unwrap();

    // No --force: no-clobber refuses outright.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--out"])
        .arg(&link)
        .assert()
        .code(2);
    assert_eq!(read_key_bytes(&key), before, "org key intact (no --force)");

    // With --force: the content backstop refuses, because the symlink
    // resolves to the org key.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "issue-cert", "--org-key"])
        .arg(&key)
        .args(["--member", MEMBER_HEX])
        .args(["--out"])
        .arg(&link)
        .arg("--force")
        .assert()
        .code(2);
    assert_eq!(
        read_key_bytes(&key),
        before,
        "org key intact (with --force)"
    );
}
