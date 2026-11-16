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

// ===========================================================================
// CLI-04 — `channel issue-grant --out` must never replace the root seed it
// loads, nor truncate whatever sits at a predictable staging name.
// ===========================================================================

/// Run `channel issue-grant` with `root` as the channel root and a second,
/// distinct identity as the issuer, writing to `out` (plus `extra` flags).
fn issue_grant(dir: &Path, root: &Path, out: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    let issuer = dir.join("issuer-node.toml");
    if !issuer.exists() {
        gen_identity(dir, "issuer-node.toml");
    }
    let issuer_hex = toml_field(&issuer, "public_key_hex").expect("identity has public_key_hex");
    net()
        .args(["channel", "issue-grant", "--root-identity"])
        .arg(root)
        .args([
            "--issuer",
            &issuer_hex,
            "--channel",
            "fleet.telemetry",
            "--out",
        ])
        .arg(out)
        .args(extra)
        .assert()
}

/// The audit's exact trigger: `--root-identity ROOT --out ROOT --force`
/// used to replace the loaded private root with public grant JSON.
#[test]
fn issue_grant_force_refuses_to_replace_its_own_root_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = gen_identity(dir.path(), "root.toml");
    let seed_before = toml_field(&root, "seed_hex").expect("root has seed_hex");

    let assert = issue_grant(dir.path(), &root, &root, &["--force"]).failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("--root-identity and --out"),
        "the refusal must name the aliased flags; got: {stderr}"
    );
    assert_eq!(
        toml_field(&root, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "the channel root seed was replaced by the grant",
    );
    assert!(!has_stage_temp(dir.path()), "refusal left a staging temp");
}

/// A spelling the lexical alias check cannot see (here a hard link; on a
/// case-insensitive filesystem a case variant behaves the same) is caught by
/// the content-based foreign-seed guard on the forced path.
#[test]
fn issue_grant_force_refuses_to_replace_any_seed_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = gen_identity(dir.path(), "root.toml");
    let seed_before = toml_field(&root, "seed_hex").expect("root has seed_hex");
    let alias = dir.path().join("grant-alias.json");
    std::fs::hard_link(&root, &alias).expect("hard link");

    let assert = issue_grant(dir.path(), &root, &alias, &["--force"]).failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("operator identity key material"),
        "the refusal must name what it found; got: {stderr}"
    );
    assert_eq!(
        toml_field(&alias, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
        "--out over a seed file replaced it",
    );
    assert_eq!(
        toml_field(&root, "seed_hex").as_deref(),
        Some(seed_before.as_str()),
    );
}

/// The old publish truncated the deterministic `<out>.tmp-channel-grant`
/// sibling (whatever it held — here a copy of a root seed) and renamed it
/// away, even with `--out` absent and no `--force`.
#[test]
fn issue_grant_never_touches_a_file_at_the_old_staging_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = gen_identity(dir.path(), "root.toml");
    let out = dir.path().join("grant.json");
    let squatter = dir.path().join("grant.tmp-channel-grant");
    std::fs::copy(&root, &squatter).expect("copy");
    let seed = toml_field(&root, "seed_hex").expect("root has seed_hex");

    issue_grant(dir.path(), &root, &out, &[]).code(0);

    assert_eq!(
        toml_field(&squatter, "seed_hex").as_deref(),
        Some(seed.as_str()),
        "the file at the old staging name was truncated or moved",
    );
    let grant = std::fs::read_to_string(&out).expect("grant published");
    assert!(grant.contains("grant_hex"), "{grant}");
    assert!(!has_stage_temp(dir.path()), "publish left a staging temp");
}

/// Positive control: `--force` still replaces an older grant atomically.
#[test]
fn issue_grant_force_still_replaces_an_older_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = gen_identity(dir.path(), "root.toml");
    let out = dir.path().join("grant.json");
    issue_grant(dir.path(), &root, &out, &[]).code(0);
    let first = std::fs::read_to_string(&out).expect("first grant");

    // Without --force the second issue is refused and the first survives.
    issue_grant(dir.path(), &root, &out, &[]).failure();
    assert_eq!(std::fs::read_to_string(&out).expect("read"), first);

    issue_grant(dir.path(), &root, &out, &["--force"]).code(0);
    let second = std::fs::read_to_string(&out).expect("second grant");
    // Each grant carries a random nonce, so a real replacement differs.
    assert_ne!(second, first, "--force left the older grant in place");
    assert!(second.contains("grant_hex"), "{second}");
    assert!(!has_stage_temp(dir.path()), "replace left a staging temp");
}
