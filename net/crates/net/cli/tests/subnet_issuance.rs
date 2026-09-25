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
    SubnetIssuerGrant, SubnetRights,
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

/// A delegated leaf is issued INSIDE its issuer's window even when the
/// requested TTL is longer than the issuer has left.
///
/// Found by CI, intermittently, at roughly one run in eight: the
/// issuer grant and the leaf are two separate `net-mesh` invocations
/// and each defaulted its window from its own `unix_now()`, so a
/// second boundary between them left the leaf's `not_after` one
/// second past its issuer's. `verify_credential_set` refuses that
/// with `IssuerAttenuationBroadened` — a delegation may not outlive
/// the grant that empowered it — and the CLI had already exited 0.
/// An issuance that succeeds and produces a credential no verifier
/// accepts is worse than a refusal.
///
/// This witness does not wait for a clock boundary. It pins the
/// issuer's window with explicit flags and asks for a leaf TTL far
/// longer than what remains, so the clamp is exercised on every run
/// rather than on an unlucky one. Without the clamp the chain fails
/// verification here deterministically.
#[test]
fn a_delegated_leaf_is_clamped_into_its_issuers_window() {
    let dir = tempfile::tempdir().unwrap();
    let root_key = subnet_keygen(dir.path(), "root.toml");
    let issuer_key = subnet_keygen(dir.path(), "issuer.toml");
    let root = entity_of(&root_key);
    let authority_hex = toml_field(&root_key, "entity_id_hex");
    let issuer_hex = toml_field(&issuer_key, "entity_id_hex");

    // The issuer's window is pinned: it starts an hour ago and has
    // one hour left.
    let issuer_not_before = unix_now().saturating_sub(3600);
    let issuer_ttl = 7200u64;
    let issuer_not_after = issuer_not_before + issuer_ttl;

    let issuer_grant = dir.path().join("issuer.grant");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-issuer", "--root-key"])
        .arg(&root_key)
        .args(["--authority", &authority_hex])
        .args(["--issuer", &issuer_hex])
        .args(["--scope", "3", "--max-rights", "attach,export"])
        .args(["--not-before", &issuer_not_before.to_string()])
        .args(["--ttl-secs", &issuer_ttl.to_string()])
        .args(["--out"])
        .arg(&issuer_grant)
        .assert()
        .code(0);

    // The leaf asks for seven days, which is far past what the issuer
    // has left.
    let delegated = dir.path().join("delegated.credential");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "issue-delegated", "--issuer-grant"])
        .arg(&issuer_grant)
        .args(["--issuer-key"])
        .arg(&issuer_key)
        .args(["--subject", SUBJECT_HEX])
        .args(["--scope", "3.9", "--rights", "export"])
        .args(["--ttl-secs", &(7 * 24 * 3600).to_string()])
        .args(["--out"])
        .arg(&delegated)
        .assert()
        .code(0);

    let set = SubnetCredentialSet::from_bytes(&std::fs::read(&delegated).unwrap()).unwrap();
    let SubnetCredentialSet::OneHop { ref leaf, .. } = set else {
        panic!("issue-delegated must produce a one-hop set");
    };
    assert!(
        leaf.not_after <= issuer_not_after,
        "the leaf's window must nest inside its issuer's: leaf not_after {} against issuer {}",
        leaf.not_after,
        issuer_not_after
    );

    // And the chain the clamp produces is one a verifier accepts —
    // the property the clamp exists for, not merely a smaller number.
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
    .expect("a clamped delegated chain verifies against the root");
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

/// A subject floor removes exactly the named rights of one full subject:
/// ATTACH by default, others only when named. The receipt says what
/// issuing did (signed) and what it did not (enforcement is pending until
/// each verifier applies it), and a floor that removes nothing is refused.
#[test]
fn subject_floor_issuance_is_exact_and_reports_enforcement_as_pending() {
    let dir = tempfile::tempdir().unwrap();
    let key = subnet_keygen(dir.path(), "root.toml");
    let root = entity_of(&key);
    let authority_hex = toml_field(&key, "entity_id_hex");
    let issue = |out: &Path, extra: &[&str]| {
        Command::cargo_bin("net-mesh")
            .unwrap()
            .args([
                "--output",
                "json",
                "subnet",
                "issue-control-fact",
                "subject-floor",
            ])
            .arg("--root-key")
            .arg(&key)
            .args(["--authority", &authority_hex])
            .args(["--scope", "3.7", "--topology-epoch", "0", "--revision", "1"])
            .args(["--subject", SUBJECT_HEX])
            .args(extra)
            .arg("--out")
            .arg(out)
            .output()
            .unwrap()
    };

    let out = dir.path().join("b.subject-floor");
    let run = issue(&out, &["--minimum-generation", "2"]);
    assert!(run.status.success(), "{run:?}");
    let receipt: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(receipt["kind"], "subject_floor");
    assert_eq!(receipt["subject_hex"], SUBJECT_HEX);
    assert_eq!(receipt["rights"], "attach", "ATTACH only unless named");
    assert!(
        receipt["enforcement"]
            .as_str()
            .unwrap()
            .starts_with("pending"),
        "issuing is not enforcement: {receipt}"
    );
    let fact = SubnetControlFact::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
    let SubnetControlFact::SubjectFloor(floor) = &fact else {
        panic!("expected a subject floor, got {:?}", fact.kind());
    };
    floor.verify().expect("signed by the root");
    assert_eq!(floor.issuer, root);
    assert_eq!(floor.rights, SubnetRights::ATTACH);
    assert_eq!(floor.minimum_generation, 2);
    assert_eq!(hex::encode(floor.subject.as_bytes()), SUBJECT_HEX);

    let wider = dir.path().join("b.route.subject-floor");
    let run = issue(
        &wider,
        &["--minimum-generation", "2", "--rights", "attach,route"],
    );
    assert!(run.status.success(), "{run:?}");
    let SubnetControlFact::SubjectFloor(floor) =
        SubnetControlFact::from_bytes(&std::fs::read(&wider).unwrap()).unwrap()
    else {
        panic!("expected a subject floor");
    };
    assert_eq!(
        floor.rights,
        SubnetRights::ATTACH.union(SubnetRights::ROUTE)
    );

    let none = dir.path().join("nothing.subject-floor");
    let run = issue(&none, &["--minimum-generation", "0"]);
    assert_eq!(run.status.code(), Some(2), "{run:?}");
    assert!(!none.exists());
}
