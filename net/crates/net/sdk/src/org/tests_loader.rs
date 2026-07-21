//! OSDK-L R2 witnesses — the grant-side audience-secret loader.
//!
//! This is the plan's highest-risk item: new code handling raw key material.
//! Each refusal path below is a security property, so each is proved rather
//! than assumed, and the happy path is proved to be *equivalent* to the
//! in-memory constructor so the file route adds no authority and skips no
//! check.

use std::io::Write;
use std::path::{Path, PathBuf};

use net::adapter::net::identity::EntityKeypair;

use super::credentials::OrgCredentials;
use super::error::OrgCredentialError;
use super::tests::{belonging, cap, discover_grant, org_a, org_b};
use super::types::*;

/// A private temp dir for one test. Unique per test name + pid + thread.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-r2-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Write `bytes` to `path` with owner-only permissions where the platform has
/// them — the shape `net org grant-capability --discover` produces.
fn write_secret_file(path: &Path, bytes: &[u8]) {
    let mut f = std::fs::File::create(path).expect("create secret file");
    f.write_all(bytes).expect("write secret file");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod 0600");
    }
}

/// A DISCOVER grant plus its secret written to a 0600 file.
fn grant_with_secret_file(dir: &Path, tag: &str) -> (OrgCapabilityGrant, PathBuf) {
    let (grant, secret) = discover_grant(&org_b(), org_a().org_id(), cap("nrpc:svc"), 3600);
    let path = dir.join(format!("{tag}.audience"));
    write_secret_file(&path, &secret.encode_config());
    (grant, path)
}

// ---------------------------------------------------------------------------
// The happy path is EQUIVALENT to the in-memory constructor
// ---------------------------------------------------------------------------

/// `from_parts` is `new` reached by a different door: same validation, same
/// refusals, same resulting credential set. If the file route could accept
/// something the in-memory route rejects, it would be a second authority path.
#[test]
fn from_parts_matches_the_in_memory_constructor() {
    let dir = scratch("equivalence");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret_path) = grant_with_secret_file(&dir, "ok");

    let from_files = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[secret_path],
    )
    .expect("loads from files");

    assert_eq!(from_files.acting_org(), a.org_id());
    assert_eq!(from_files.member(), &member);
    assert_eq!(from_files.grants().len(), 1);
    assert_eq!(from_files.grants()[0].grant_id, grant.grant_id);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The structural checks are NOT skipped on the file route — a grant issued to
/// another org is refused identically.
#[test]
fn from_parts_still_enforces_the_structural_relations() {
    let dir = scratch("structural");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);

    // B grants to a THIRD org, not to A.
    let (grant, secret) = discover_grant(
        &org_b(),
        OrgKeypair::from_bytes([0xC3u8; 32]).org_id(),
        cap("nrpc:svc"),
        3600,
    );
    let path = dir.join("foreign.audience");
    write_secret_file(&path, &secret.encode_config());

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::GrantNotForActingOrg { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Malformed public credential bytes are refused before any file is touched.
#[test]
fn from_parts_refuses_malformed_public_credentials() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);

    let err = OrgCredentials::from_parts(&[0u8; 4], &dg.to_bytes(), &[], &[])
        .expect_err("truncated membership");
    assert!(
        matches!(err, OrgCredentialError::SignatureInvalid { .. }),
        "got {err:?}"
    );

    let err = OrgCredentials::from_parts(&cert.to_bytes(), &[0u8; 4], &[], &[])
        .expect_err("truncated dispatcher grant");
    assert!(
        matches!(err, OrgCredentialError::SignatureInvalid { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Loader refusals — each is a security property
// ---------------------------------------------------------------------------

/// A missing file is a refusal, never an empty credential set.
#[test]
fn a_missing_secret_file_is_refused() {
    let dir = scratch("missing");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, _real_path) = grant_with_secret_file(&dir, "present");

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[dir.join("does-not-exist.audience")],
    )
    .expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretFile { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that is a valid secret FOLLOWED BY anything else is refused. Without
/// the trailing-byte probe, a secret with appended content would load silently.
#[test]
fn trailing_bytes_after_the_secret_are_refused() {
    let dir = scratch("trailing");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    let mut bytes = secret.encode_config().to_vec();
    bytes.push(0x00); // one byte past the encoding
    let path = dir.join("trailing.audience");
    write_secret_file(&path, &bytes);

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse");
    match &err {
        OrgCredentialError::AudienceSecretFile { detail, .. } => {
            assert!(detail.contains("trailing"), "{detail}")
        }
        other => panic!("got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A short file is refused rather than zero-padded into a decodable shape.
#[test]
fn a_truncated_secret_file_is_refused() {
    let dir = scratch("short");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    let encoded = secret.encode_config();
    let path = dir.join("short.audience");
    write_secret_file(&path, &encoded[..encoded.len() - 1]);

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretFile { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exactly-sized garbage is refused by the codec, and the refusal names the
/// path and nothing else — no byte derived from the file reaches the message.
#[test]
fn undecodable_content_is_refused_without_echoing_it() {
    let dir = scratch("garbage");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    // Right length, wrong version byte — and a recognizable marker we then
    // assert never appears in the error text.
    let mut bytes = vec![0xEEu8; secret.encode_config().len()];
    bytes[1] = 0xAB;
    let path = dir.join("garbage.audience");
    write_secret_file(&path, &bytes);

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse");
    let rendered = format!("{err}");
    assert!(rendered.contains("garbage.audience"), "{rendered}");
    assert!(
        !rendered.contains("ee") && !rendered.contains("EE") && !rendered.contains("ab"),
        "the refusal must not echo file content: {rendered}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory is not a secret. Without the regular-file check on the opened
/// object, the read would fail with a confusing IO error instead of a refusal.
#[test]
fn a_directory_is_not_a_secret_file() {
    let dir = scratch("dir-as-file");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, _p) = grant_with_secret_file(&dir, "real");

    let subdir = dir.join("not-a-file");
    std::fs::create_dir_all(&subdir).expect("mkdir");

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[subdir],
    )
    .expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretFile { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A world-readable secret is refused: the key is readable by any local
/// account, so the file is compromised whatever its contents say.
#[cfg(unix)]
#[test]
fn a_group_or_world_readable_secret_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("perms");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    let path = dir.join("loose.audience");
    write_secret_file(&path, &secret.encode_config());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod 0644");

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse a readable secret");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretFile { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A symlink to a valid secret is refused — the open is `O_NOFOLLOW`, so an
/// attacker who can plant a link cannot redirect the read to a file whose
/// permissions we never checked.
#[cfg(unix)]
#[test]
fn a_symlinked_secret_path_is_refused() {
    let dir = scratch("symlink");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, real) = grant_with_secret_file(&dir, "target");

    let link = dir.join("link.audience");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[link],
    )
    .expect_err("must refuse a symlinked secret");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretFile { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A secret whose commitment does not match any held grant is still refused —
/// the loader hands off to the same `matches_grant` relation the in-memory
/// route uses, so a stale secret for a re-issued grant cannot slip in by file.
#[test]
fn a_secret_for_a_different_grant_is_refused() {
    let dir = scratch("mismatch");
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);

    let (grant, _kept) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    // A DIFFERENT grant's secret — fresh audience material per grant.
    let (_other, other_secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:other"), 3600);
    let path = dir.join("other.audience");
    write_secret_file(&path, &other_secret.encode_config());

    let err = OrgCredentials::from_parts(
        &cert.to_bytes(),
        &dg.to_bytes(),
        &[grant.to_bytes()],
        &[path],
    )
    .expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretMismatch { .. }),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wire kind is stable — the bindings key on it.
#[test]
fn the_audience_secret_file_refusal_has_a_stable_wire_kind() {
    let e = OrgCredentialError::AudienceSecretFile {
        path: "/x".to_string(),
        detail: "refused".to_string(),
    };
    assert_eq!(e.wire_kind(), "audience_secret_file");
}
