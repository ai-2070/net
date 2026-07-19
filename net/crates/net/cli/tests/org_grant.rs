//! OA-2 grant CLI flow — `net org (grant-dispatcher|grant-capability)`.
//! Drives the real `net-mesh` binary against a tempdir: grants are
//! written as versioned JSON envelopes, overwrite is refused, scope /
//! rights / target selection is validated, a `--discover` grant mints a
//! 0600 audience secret, and the written secret + grant are a consistent
//! pair under `matches_grant`.

use assert_cmd::prelude::*;
use net_sdk::org::{OrgAudienceSecret, OrgCapabilityGrant};
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

/// Extract `org_id_hex` from a `net org keygen` TOML key file.
fn org_id_of(key: &Path) -> String {
    let text = std::fs::read_to_string(key).unwrap();
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("org_id_hex") {
            return rest
                .trim_start_matches(['=', ' '])
                .trim()
                .trim_matches('"')
                .to_string();
        }
    }
    panic!("org_id_hex not found in {}", key.display());
}

/// True if any `*.stage.*` publish temp was left behind in `dir`.
fn has_stage_temp(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().contains(".stage."))
}

/// A stable fake dispatcher entity id (any 32 bytes decode as an
/// EntityId; issuance only binds the public half).
const DISPATCHER_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";

#[test]
fn grant_dispatcher_writes_grant_and_refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("dispatcher.grant.json");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(0);

    let text = std::fs::read_to_string(&grant).unwrap();
    assert!(
        text.contains("\"version\": 1"),
        "envelope carries version 1: {text}"
    );
    assert!(
        text.contains("\"grant\""),
        "envelope carries the grant field"
    );

    // Refuses to clobber without --force.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(2);
}

#[test]
fn grant_dispatcher_any_capability_and_scope_validation() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");

    // `--any-capability` alone → OK.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(dir.path().join("any.grant.json"))
        .assert()
        .code(0);

    // Both `--capability` and `--any-capability` → refused.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--any-capability")
        .args(["--out"])
        .arg(dir.path().join("both.grant.json"))
        .assert()
        .code(2);

    // Neither scope flag → refused.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .args(["--out"])
        .arg(dir.path().join("neither.grant.json"))
        .assert()
        .code(2);
}

/// Stable fake org / node ids (any 32 bytes are a valid OrgId /
/// EntityId; issuance binds the value, not a live key).
const GRANTEE_ORG_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TARGET_ORG_HEX: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const TARGET_NODE_HEX: &str = "3333333333333333333333333333333333333333333333333333333333333333";

#[test]
fn grant_capability_invoke_only_writes_grant_without_secret() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("cap.grant.json");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(0);

    let text = std::fs::read_to_string(&grant).unwrap();
    assert!(
        text.contains("\"version\": 1"),
        "envelope carries version 1"
    );
    assert!(
        text.contains("\"grant\""),
        "envelope carries the grant field"
    );
    // An INVOKE-only grant mints no audience secret.
    assert!(!dir.path().join("cap.audience.key").exists());
}

#[test]
fn grant_capability_discover_mints_secret_file_0600() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("cap.grant.json");
    let secret = dir.path().join("cap.audience.key");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&grant)
        .args(["--audience-out"])
        .arg(&secret)
        .assert()
        .code(0);

    assert!(grant.exists(), "grant file written");
    let secret_bytes = std::fs::read(&secret).unwrap();
    assert_eq!(
        secret_bytes.len(),
        97,
        "audience secret is the canonical 97-byte encode_config",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "audience secret must be owner-only, got {mode:o}"
        );
    }
}

#[test]
fn grant_capability_flag_validation() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let out = dir.path().join("x.grant.json");

    // Each of these fails validation BEFORE any file is written.
    for extra in [
        // no rights
        vec!["--target-any-owned-by", TARGET_ORG_HEX],
        // --discover without --audience-out
        vec![
            "--invoke",
            "--discover",
            "--target-any-owned-by",
            TARGET_ORG_HEX,
        ],
        // both target flags
        vec![
            "--invoke",
            "--target-node",
            TARGET_NODE_HEX,
            "--target-any-owned-by",
            TARGET_ORG_HEX,
        ],
        // neither target flag
        vec!["--invoke"],
    ] {
        let mut c = Command::cargo_bin("net-mesh").unwrap();
        c.args(["org", "grant-capability", "--org-key"])
            .arg(&key)
            .args(["--grantee-org", GRANTEE_ORG_HEX])
            .args(["--capability", "nrpc:svc"])
            .args(["--out"])
            .arg(&out);
        for a in &extra {
            c.arg(a);
        }
        c.assert().code(2);
    }

    // --audience-out without --discover → refused.
    let stray_secret = dir.path().join("stray.key");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-any-owned-by", TARGET_ORG_HEX])
        .args(["--out"])
        .arg(&out)
        .args(["--audience-out"])
        .arg(&stray_secret)
        .assert()
        .code(2);
    assert!(
        !stray_secret.exists(),
        "no secret written on a rejected run"
    );
}

#[test]
fn grant_capability_discover_artifacts_are_a_consistent_pair() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant_path = dir.path().join("cap.grant.json");
    let secret_path = dir.path().join("cap.audience.key");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&grant_path)
        .args(["--audience-out"])
        .arg(&secret_path)
        .assert()
        .code(0);

    // Load both artifacts through the SDK and confirm they are a consistent
    // pair via the whole-object `matches_grant` primitive — not by manually
    // remembering part of the relation.
    let envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
    let grant_hex = envelope["grant"].as_str().expect("grant hex in envelope");
    let grant = OrgCapabilityGrant::from_bytes(&hex::decode(grant_hex).unwrap())
        .expect("decode grant from CLI envelope");
    let secret = OrgAudienceSecret::decode_config(&std::fs::read(&secret_path).unwrap())
        .expect("decode audience secret from CLI file");

    assert!(
        secret.matches_grant(&grant),
        "the CLI-written audience secret matches its grant",
    );
}

#[test]
fn malformed_org_key_error_never_echoes_the_seed() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("org.toml");
    // A recognizable sentinel seed on a MALFORMED line (bare hex is not a valid
    // TOML value): pre-fix, the toml parse error echoed this line — including the
    // seed — verbatim to stderr.
    const SENTINEL: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef01";
    std::fs::write(
        &key,
        format!("org_id_hex = \"00\"\nseed_hex = {SENTINEL}\n"),
    )
    .unwrap();

    let assert = Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(dir.path().join("g.json"))
        .arg("--insecure-permissions")
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        !stderr.contains(SENTINEL),
        "the org-key parse error leaked the seed sentinel to stderr: {stderr}",
    );
}

#[test]
fn grant_capability_rejects_foreign_owner_target() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("foreign.grant.json");
    // TARGET_ORG_HEX is not the keygen'd issuer org, so `AnyNodeOwnedBy(foreign)`
    // is permanently unusable (admission requires the provider's owner == issuer)
    // and is refused locally.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-any-owned-by", TARGET_ORG_HEX])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(2);
    assert!(
        !grant.exists(),
        "no grant written on a rejected foreign-owner target"
    );
}

#[test]
fn grant_capability_any_node_owned_by_self_admits() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let issuer_org = org_id_of(&key);
    let grant = dir.path().join("self.grant.json");
    // AnyNodeOwnedBy(issuer) is the valid form — the issuer grants access to
    // nodes it owns.
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-any-owned-by", issuer_org.as_str()])
        .args(["--out"])
        .arg(&grant)
        .assert()
        .code(0);
    assert!(grant.exists(), "AnyNodeOwnedBy(issuer) is a valid grant");
}

#[test]
fn grant_capability_rejects_aliased_paths() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let same = dir.path().join("same.json");

    // --out aliased onto --audience-out → refused (would collide the pair).
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&same)
        .args(["--audience-out"])
        .arg(&same)
        .assert()
        .code(2);

    // --out aliased onto the org key → refused (would clobber the root seed).
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&key)
        .assert()
        .code(2);
    assert!(
        std::fs::read_to_string(&key).unwrap().contains("seed_hex"),
        "the org key was not clobbered",
    );
}

#[test]
fn grant_capability_force_with_discover_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(dir.path().join("g.json"))
        .args(["--audience-out"])
        .arg(dir.path().join("s.key"))
        .arg("--force")
        .assert()
        .code(2);
}

// --force is refused for grant-capability even without --discover (it used to
// force-replace a single output). Publication is no-clobber; a forced replace is
// not crash-atomic (Kyra OA2-F).
#[test]
fn grant_capability_force_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(dir.path().join("g.json"))
        .arg("--force")
        .assert()
        .code(2);
}

// P1 regression: a forced `--out` aimed at a CASE-VARIANT of the org key
// (`ORG.TOML` vs `org.toml`) must never destroy the root. `--force` is refused
// before any filesystem work, so the root survives on EVERY platform — not just
// case-sensitive ones (Kyra OA2-F closure-2).
#[test]
fn grant_dispatcher_force_refusal_preserves_a_case_variant_root() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let case_alias = dir.path().join("ORG.TOML");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(&case_alias)
        .arg("--force")
        .assert()
        .code(2);

    // The org root key is intact — never replaced by grant JSON.
    assert!(
        std::fs::read_to_string(&key).unwrap().contains("seed_hex"),
        "the org root key was not clobbered through a forced case-variant alias",
    );
}

// On a case-insensitive filesystem `--out ORG.TOML` collides with the existing
// `org.toml` root key even without --force: the case-sensitive alias guard does
// not catch it, but the no-clobber hard-link publish refuses the collision, so
// the root survives and no stage temp is left behind (Kyra OA2-F closure-2).
#[cfg(windows)]
#[test]
fn grant_dispatcher_case_variant_no_clobber_preserves_root() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let case_alias = dir.path().join("ORG.TOML");

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(&case_alias)
        .assert()
        .code(2);

    assert!(
        std::fs::read_to_string(&key).unwrap().contains("seed_hex"),
        "the case-variant no-clobber collision preserved the org root key",
    );
    assert!(
        !has_stage_temp(dir.path()),
        "no .stage. temp remains after the refused case-variant collision",
    );
}

#[test]
fn grant_capability_pair_rollback_leaves_no_grant_when_secret_publish_fails() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let grant = dir.path().join("cap.grant.json");
    let secret = dir.path().join("cap.audience.key");
    // Pre-create the audience-out path so the SECOND publish (secret) fails
    // no-clobber AFTER the grant is published — forcing a rollback of the grant.
    std::fs::write(&secret, b"preexisting").unwrap();

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-capability", "--org-key"])
        .arg(&key)
        .args(["--grantee-org", GRANTEE_ORG_HEX])
        .args(["--capability", "nrpc:svc"])
        .arg("--invoke")
        .arg("--discover")
        .args(["--target-node", TARGET_NODE_HEX])
        .args(["--out"])
        .arg(&grant)
        .args(["--audience-out"])
        .arg(&secret)
        .assert()
        .code(2);

    assert!(
        !grant.exists(),
        "the grant was rolled back after the secret publish failed"
    );
    assert_eq!(
        std::fs::read(&secret).unwrap(),
        b"preexisting",
        "the pre-existing secret file was left untouched",
    );
    assert!(!has_stage_temp(dir.path()), "no .stage. temp files remain");
}

// A leaf symlink at the output path is never followed or truncated — the
// no-clobber publish refuses (Unix; the CLI has no clean Windows analog).
#[cfg(unix)]
#[test]
fn grant_dispatcher_does_not_follow_a_leaf_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"important").unwrap();
    let out = dir.path().join("g.json");
    std::os::unix::fs::symlink(&victim, &out).unwrap();

    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["org", "grant-dispatcher", "--org-key"])
        .arg(&key)
        .args(["--dispatcher", DISPATCHER_HEX])
        .arg("--any-capability")
        .args(["--out"])
        .arg(&out)
        .assert()
        .code(2);
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"important",
        "the symlink target was not truncated through the leaf",
    );
}

// On Windows the 0600 mode is not enforced, so a --discover run warns about the
// inherited DACL by default and is silenced by --insecure-permissions.
#[cfg(windows)]
#[test]
fn grant_capability_discover_warns_about_windows_dacl() {
    let dir = tempfile::tempdir().unwrap();
    let key = keygen(dir.path(), "org.toml");
    let run = |extra: &[&str], name: &str| -> String {
        let assert = Command::cargo_bin("net-mesh")
            .unwrap()
            .args(["org", "grant-capability", "--org-key"])
            .arg(&key)
            .args(["--grantee-org", GRANTEE_ORG_HEX])
            .args(["--capability", "nrpc:svc"])
            .arg("--invoke")
            .arg("--discover")
            .args(["--target-node", TARGET_NODE_HEX])
            .args(["--out"])
            .arg(dir.path().join(format!("{name}.grant.json")))
            .args(["--audience-out"])
            .arg(dir.path().join(format!("{name}.key")))
            .args(extra)
            .assert()
            .code(0);
        String::from_utf8_lossy(&assert.get_output().stderr).to_string()
    };
    assert!(
        run(&[], "warned").contains("not enforced on Windows"),
        "a --discover run warns about the Windows DACL by default",
    );
    assert!(
        !run(&["--insecure-permissions"], "silent").contains("not enforced on Windows"),
        "--insecure-permissions silences the Windows DACL warning",
    );
}
