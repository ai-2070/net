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
