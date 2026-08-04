//! SSDK S3 — `net-mesh subnet (keygen|issue-*|inspect)` offline
//! provisioning flow, driven through the real binary against tempdirs.
//!
//! The strong witnesses are ROUND TRIPS through the core verifier: an
//! issued artifact file decodes with the core `from_bytes` and passes
//! `verify_credential_set` (or the fact's own signature check) against
//! the root it names — proving the CLI writes canonical wire bytes, not
//! a mirror. Refusals (overwrite, foreign-seed replace, scope escape,
//! rights widening, malformed inspect) exit non-zero and leave no
//! partial output.

use assert_cmd::prelude::*;
use net::adapter::net::identity::EntityId;
use net::adapter::net::subnet::auth::verify_credential_set;
use net::adapter::net::subnet::{
    SubnetAuthorityConfig, SubnetControlFact, SubnetCredentialSet, SubnetFloorRegistry,
    SubnetIssuerGrant,
};
use std::path::Path;
use std::process::Command;

fn subnet_keygen(dir: &Path, name: &str) -> std::path::PathBuf {
    let key = dir.join(name);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(0);
    key
}

/// Extract a TOML string field from a key file.
fn toml_field(key: &Path, field: &str) -> String {
    let text = std::fs::read_to_string(key).unwrap();
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix(field) {
            return rest
                .trim_start_matches(['=', ' '])
                .trim()
                .trim_matches('"')
                .to_string();
        }
    }
    panic!("{field} not found in {}", key.display());
}

fn entity_of(key: &Path) -> EntityId {
    let hex = toml_field(key, "entity_id_hex");
    let bytes: [u8; 32] = hex::decode(hex).unwrap().try_into().unwrap();
    EntityId::from_bytes(bytes)
}

fn has_stage_temp(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().contains(".stage."))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

const SUBJECT_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

#[test]
fn keygen_writes_marked_key_refuses_overwrite_and_never_prints_the_seed() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("root.toml");

    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(toml_field(&key, "kind"), "subnet-authority-key");
    let seed_hex = toml_field(&key, "seed_hex");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(&seed_hex) && !stderr.contains(&seed_hex),
        "keygen must never print the seed",
    );

    // Overwrite refused without --force; allowed with it (same kind).
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--out"])
        .arg(&key)
        .assert()
        .failure();
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--force", "--out"])
        .arg(&key)
        .assert()
        .code(0);

    // A DIFFERENT kind of secret is never replaced, force or not.
    let org_key = dir.path().join("org.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "keygen", "--out"])
        .arg(&org_key)
        .assert()
        .code(0);
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--force", "--out"])
        .arg(&org_key)
        .assert()
        .failure();

    assert!(!has_stage_temp(dir.path()), "no stage temps left behind");
}

#[test]
fn issue_direct_writes_wire_bytes_the_core_verifier_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let key = subnet_keygen(dir.path(), "root.toml");
    let root = entity_of(&key);
    let authority_hex = toml_field(&key, "entity_id_hex");
    let out = dir.path().join("gateway.credential");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-direct", "--root-key"])
        .arg(&key)
        .args(["--authority", &authority_hex])
        .args(["--subject", SUBJECT_HEX])
        .args(["--scope", "3.9", "--rights", "export"])
        .args(["--out"])
        .arg(&out)
        .assert()
        .code(0);

    // The file IS the canonical framed wire form, and it verifies.
    let bytes = std::fs::read(&out).unwrap();
    let set = SubnetCredentialSet::from_bytes(&bytes).expect("canonical frame");
    assert!(matches!(set, SubnetCredentialSet::Direct(_)));
    let subject = EntityId::from_bytes(hex::decode(SUBJECT_HEX).unwrap().try_into().unwrap());
    let config = SubnetAuthorityConfig {
        authority: root.clone(),
        roots: vec![root],
        maximum_grant_lifetime_secs: 30 * 24 * 60 * 60,
    };
    verify_credential_set(
        &set,
        &subject,
        &config,
        0,
        &SubnetFloorRegistry::new(),
        unix_now(),
        0,
    )
    .expect("the issued credential verifies against its root");

    // Overwrite refused without --force.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-direct", "--root-key"])
        .arg(&key)
        .args(["--authority", &authority_hex])
        .args(["--subject", SUBJECT_HEX])
        .args(["--scope", "3.9", "--rights", "export"])
        .args(["--out"])
        .arg(&out)
        .assert()
        .failure();
    assert!(!has_stage_temp(dir.path()));
}

#[test]
fn issue_delegated_chain_verifies_and_refuses_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let root_key = subnet_keygen(dir.path(), "root.toml");
    let issuer_key = subnet_keygen(dir.path(), "issuer.toml");
    let root = entity_of(&root_key);
    let authority_hex = toml_field(&root_key, "entity_id_hex");
    let issuer_hex = toml_field(&issuer_key, "entity_id_hex");

    let issuer_grant = dir.path().join("issuer.grant");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-issuer", "--root-key"])
        .arg(&root_key)
        .args(["--authority", &authority_hex])
        .args(["--issuer", &issuer_hex])
        .args(["--scope", "3", "--max-rights", "attach,export"])
        .args(["--out"])
        .arg(&issuer_grant)
        .assert()
        .code(0);
    SubnetIssuerGrant::from_bytes(&std::fs::read(&issuer_grant).unwrap())
        .expect("issuer grant is the canonical wire form");

    // The happy path: leaf inside scope, rights within maximum.
    let delegated = dir.path().join("delegated.credential");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-delegated", "--issuer-grant"])
        .arg(&issuer_grant)
        .args(["--issuer-key"])
        .arg(&issuer_key)
        .args(["--subject", SUBJECT_HEX])
        .args(["--scope", "3.9", "--rights", "export"])
        .args(["--out"])
        .arg(&delegated)
        .assert()
        .code(0);
    let set = SubnetCredentialSet::from_bytes(&std::fs::read(&delegated).unwrap()).unwrap();
    assert!(matches!(set, SubnetCredentialSet::OneHop { .. }));
    let subject = EntityId::from_bytes(hex::decode(SUBJECT_HEX).unwrap().try_into().unwrap());
    let config = SubnetAuthorityConfig {
        authority: root.clone(),
        roots: vec![root],
        maximum_grant_lifetime_secs: 30 * 24 * 60 * 60,
    };
    verify_credential_set(
        &set,
        &subject,
        &config,
        0,
        &SubnetFloorRegistry::new(),
        unix_now(),
        0,
    )
    .expect("the delegated chain verifies against the root");

    // Scope escape and rights widening refuse with nothing written.
    for (scope, rights) in [("4.1", "export"), ("3.9", "route")] {
        let bad = dir.path().join(format!("bad-{}.credential", rights));
        Command::cargo_bin("net-mesh")
            .unwrap()
            .args(["subnet", "issue-delegated", "--issuer-grant"])
            .arg(&issuer_grant)
            .args(["--issuer-key"])
            .arg(&issuer_key)
            .args(["--subject", SUBJECT_HEX])
            .args(["--scope", scope, "--rights", rights])
            .args(["--out"])
            .arg(&bad)
            .assert()
            .failure();
        assert!(!bad.exists(), "a refused issuance must write nothing");
    }
    assert!(!has_stage_temp(dir.path()));
}

#[test]
fn control_facts_frame_correctly_and_inspect_classifies_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let key = subnet_keygen(dir.path(), "root.toml");
    let root = entity_of(&key);
    let authority_hex = toml_field(&key, "entity_id_hex");

    let floor = dir.path().join("perception.floor");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "subnet",
            "issue-control-fact",
            "revocation-floor",
            "--root-key",
        ])
        .arg(&key)
        .args(["--authority", &authority_hex])
        .args(["--scope", "3.9", "--topology-epoch", "0"])
        .args(["--revision", "1", "--minimum-generation", "2"])
        .args(["--out"])
        .arg(&floor)
        .assert()
        .code(0);
    let fact = SubnetControlFact::from_bytes(&std::fs::read(&floor).unwrap())
        .expect("the file is the OUTER control-fact frame, not a raw inner object");
    let SubnetControlFact::RevocationFloor(inner) = &fact else {
        panic!("expected a revocation floor, got {:?}", fact.kind());
    };
    inner.verify().expect("the floor's signature verifies");
    assert_eq!(
        inner.issuer, root,
        "the floor names the signing root as its issuer",
    );
    assert_eq!(inner.minimum_generation, 2);

    // Inspect classifies every artifact kind and exits 0…
    let cred = dir.path().join("gateway.credential");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-direct", "--root-key"])
        .arg(&key)
        .args(["--authority", &authority_hex])
        .args(["--subject", SUBJECT_HEX])
        .args(["--scope", "3.9", "--rights", "export"])
        .args(["--out"])
        .arg(&cred)
        .assert()
        .code(0);
    for artifact in [&floor, &cred] {
        Command::cargo_bin("net-mesh")
            .unwrap()
            .args(["subnet", "inspect"])
            .arg(artifact)
            .assert()
            .code(0);
    }

    // …and refuses malformed bytes non-zero.
    let garbage = dir.path().join("garbage.bin");
    std::fs::write(&garbage, [0xFFu8; 41]).unwrap();
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "inspect"])
        .arg(&garbage)
        .assert()
        .failure();

    // Inspect never prints the seed even when pointed at a KEY file.
    let seed_hex = toml_field(&key, "seed_hex");
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "inspect"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(!out.status.success(), "a key file is not a wire artifact");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains(&seed_hex), "inspect must never print a seed");
}
