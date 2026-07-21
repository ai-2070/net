//! §2 — no verb may destroy a DIFFERENT kind of secret than the one it writes.
//!
//! The 2026-07-20 review filed §2 against `issue-cert --force` /
//! `issue-floors --force`, which could truncate the org root key. That fix
//! landed a content-sniffing backstop (`refuse_replacing_foreign_seed`, then
//! named `refuse_replacing_org_key`) — but wired it into
//! `publish_json_artifact` only, which the two verbs that themselves WRITE
//! seed files never call. So the identical end state stayed reachable:
//!
//! ```text
//! net identity generate --out "$ORG_KEY" --force   # exit 0, org root gone
//! ```
//!
//! `net org keygen --force` had the same shape, plus a TOCTOU: it published
//! through the identity helper, whose `create_new` guards only the temp and
//! whose `rename` always replaces, so its `try_exists` pre-check was its only
//! clobber protection despite `refuse_existing`'s docstring disclaiming exactly
//! that role.
//!
//! The rule these tests pin: **`--force` may replace an artifact of its own
//! kind, and must never replace a different kind of secret.** Each refusal case
//! below is paired with the same-kind positive control, so a regression that
//! simply refuses everything cannot pass this file.

use assert_cmd::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn net() -> Command {
    Command::cargo_bin("net-mesh").expect("net-mesh binary")
}

fn keygen(dir: &Path, name: &str) -> PathBuf {
    let key = dir.join(name);
    net()
        .args(["org", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(0);
    key
}

fn gen_identity(dir: &Path, name: &str) -> PathBuf {
    let id = dir.join(name);
    net()
        .args(["identity", "generate", "--out"])
        .arg(&id)
        .assert()
        .code(0);
    id
}

/// Read a `key = "value"` field out of one of the TOML secret files.
fn toml_field(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// True if any `*.stage.*` publish temp was left behind.
fn has_stage_temp(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().contains(".stage."))
        })
        .unwrap_or(false)
}

// ===========================================================================
// The §2 witness: an identity must never land on the org root.
// ===========================================================================

#[test]
fn identity_generate_force_refuses_to_replace_the_org_root_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = keygen(dir.path(), "org.toml");

    let org_id_before = toml_field(&key, "org_id_hex").expect("org key has org_id_hex");
    let seed_before = toml_field(&key, "seed_hex").expect("org key has seed_hex");

    // The exact drifted-variable shape §2 describes.
    let assert = net()
        .args(["identity", "generate", "--force", "--out"])
        .arg(&key)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("org root key material"),
        "the refusal must name WHAT it found, so the operator knows the \
         --out drifted rather than that the command is broken; got: {stderr}"
    );

    // The org root is untouched — byte-identical, not merely present.
    assert_eq!(
        toml_field(&key, "org_id_hex").as_deref(),
        Some(org_id_before.as_str()),
        "the org key's identity changed — it was replaced",
    );
    assert_eq!(
        toml_field(&key, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "the org root SEED changed — this is the §2 end state",
    );
    assert!(
        toml_field(&key, "operator_id_hex").is_none(),
        "an operator identity was written over the org key",
    );
    assert!(!has_stage_temp(dir.path()), "refusal left a staging temp");
}

/// Positive control for the above: the guard must not break rotation.
#[test]
fn identity_generate_force_still_rotates_an_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = gen_identity(dir.path(), "operator.toml");
    let seed_before = toml_field(&id, "seed_hex").expect("identity has seed_hex");

    net()
        .args(["identity", "generate", "--force", "--out"])
        .arg(&id)
        .assert()
        .code(0);

    let seed_after = toml_field(&id, "seed_hex").expect("identity still has seed_hex");
    assert_ne!(
        seed_before, seed_after,
        "--force over an identity must actually rotate it",
    );
    assert!(!has_stage_temp(dir.path()), "rotation left a staging temp");
}

// ===========================================================================
// The mirror: an org key must never land on an operator identity.
// ===========================================================================

#[test]
fn org_keygen_force_refuses_to_replace_an_operator_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = gen_identity(dir.path(), "operator.toml");
    let seed_before = toml_field(&id, "seed_hex").expect("identity has seed_hex");

    let assert = net()
        .args(["org", "keygen", "--force", "--out"])
        .arg(&id)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("operator identity key material"),
        "the refusal must name what it found; got: {stderr}"
    );

    assert_eq!(
        toml_field(&id, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "the operator identity seed was replaced",
    );
    assert!(
        toml_field(&id, "org_id_hex").is_none(),
        "an org key was written over the operator identity",
    );
}

/// Positive control: `keygen --force` over an org key is the legitimate
/// same-kind replace the flag exists for.
#[test]
fn org_keygen_force_replaces_its_own_org_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = keygen(dir.path(), "org.toml");
    let id_before = toml_field(&key, "org_id_hex").expect("org key has org_id_hex");

    net()
        .args(["org", "keygen", "--force", "--out"])
        .arg(&key)
        .assert()
        .code(0);

    let id_after = toml_field(&key, "org_id_hex").expect("org key still has org_id_hex");
    assert_ne!(
        id_before, id_after,
        "--force over an org key must mint a NEW org",
    );
    assert!(!has_stage_temp(dir.path()), "replace left a staging temp");
}

// ===========================================================================
// keygen's no-clobber boundary is now the publish, not a TOCTOU stat.
// ===========================================================================

#[test]
fn org_keygen_without_force_never_clobbers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = keygen(dir.path(), "org.toml");
    let seed_before = toml_field(&key, "seed_hex").expect("org key has seed_hex");

    net()
        .args(["org", "keygen", "--out"])
        .arg(&key)
        .assert()
        .failure();

    assert_eq!(
        toml_field(&key, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "a refused keygen still replaced the key",
    );
    assert!(!has_stage_temp(dir.path()), "refusal left a staging temp");
}

/// keygen must not write THROUGH a symlink. Before the staged publish it ended
/// in a bare `rename`, which replaces the link itself — but the pre-check
/// `try_exists` follows links, so the two disagreed about what was being
/// guarded. Staging beside the destination and hard-linking makes the leaf the
/// thing that is checked.
#[cfg(unix)]
#[test]
fn org_keygen_does_not_write_through_a_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = keygen(dir.path(), "real-org.toml");
    let seed_before = toml_field(&real, "seed_hex").expect("org key has seed_hex");

    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    // No --force: the no-clobber publish must refuse rather than follow.
    net()
        .args(["org", "keygen", "--out"])
        .arg(&link)
        .assert()
        .failure();

    assert_eq!(
        toml_field(&real, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "keygen wrote through the symlink and replaced the target org key",
    );
}
