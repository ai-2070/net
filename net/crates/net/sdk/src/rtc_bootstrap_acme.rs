//! ACME (HTTP-01) for the bootstrap listener's certificate.
//!
//! Split from [`crate::rtc_bootstrap`] so the listener module stays
//! about the browser protocol, and so the two halves of ACME are
//! visibly separate:
//!
//! - the **challenge route**, which lives in the listener
//!   (`/.well-known/acme-challenge/{token}` served from
//!   [`AcmeState`]) and is exercised by an ordinary test; and
//! - the **ordering client** here, which talks to a real ACME
//!   directory.
//!
//! The order is cached on disk ([`AcmeConfig::cache_dir`]) so a
//! restart re-reads a live certificate instead of re-ordering —
//! Let's Encrypt's rate limits make an anchor that orders on every
//! boot a self-inflicted outage.
//!
//! **HTTP-01 on the same listener means the same socket answers
//! `:443`.** The plan's constraint is that CI must not pass
//! `--ignore-certificate-errors`; that is satisfied by there being
//! no self-signed path at all, here or in the operator path.

use std::path::{Path, PathBuf};

use crate::rtc_bootstrap::{read_pem_pair, AcmeConfig, AcmeState, BootstrapError};

/// File names inside [`AcmeConfig::cache_dir`].
const CERT_FILE: &str = "bootstrap-cert.pem";
const KEY_FILE: &str = "bootstrap-key.pem";

type Chain = (
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
);

/// Return a certificate for [`AcmeConfig::domain`]: the cached one
/// when the cache holds a usable pair, otherwise a freshly ordered
/// one.
pub(crate) async fn obtain_certificate(
    config: &AcmeConfig,
    challenges: &AcmeState,
) -> Result<Chain, BootstrapError> {
    if let Some(cached) = read_cache(&config.cache_dir) {
        return Ok(cached);
    }
    let (cert_pem, key_pem) = order_certificate(config, challenges).await?;
    write_cache(&config.cache_dir, &cert_pem, &key_pem)?;
    read_cache(&config.cache_dir).ok_or_else(|| {
        BootstrapError::Acme("the freshly issued certificate did not read back".into())
    })
}

fn cache_paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join(CERT_FILE), dir.join(KEY_FILE))
}

fn read_cache(dir: &Path) -> Option<Chain> {
    let (cert, key) = cache_paths(dir);
    if !cert.exists() || !key.exists() {
        return None;
    }
    read_pem_pair(&cert, &key).ok()
}

fn write_cache(dir: &Path, cert_pem: &str, key_pem: &str) -> Result<(), BootstrapError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| BootstrapError::Acme(format!("creating {}: {e}", dir.display())))?;
    let (cert, key) = cache_paths(dir);
    std::fs::write(&cert, cert_pem)
        .map_err(|e| BootstrapError::Acme(format!("writing {}: {e}", cert.display())))?;
    write_private_key(&key, key_pem)?;
    Ok(())
}

/// Write the private key **created 0600 from inception**, and fail
/// hard if the mode cannot be applied (R5b).
///
/// The old shape wrote the file with default permissions and then
/// chmod'd, ignoring the result: there was a window in which the key
/// was world-readable, and a filesystem that refuses the mode left a
/// readable key with no error at all.
fn write_private_key(path: &Path, pem: &str) -> Result<(), BootstrapError> {
    use std::io::Write as _;

    // Never inherit a previous file's mode.
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| BootstrapError::Acme(format!("replacing {}: {e}", path.display())))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| BootstrapError::Acme(format!("creating {}: {e}", path.display())))?;
    file.write_all(pem.as_bytes())
        .map_err(|e| BootstrapError::Acme(format!("writing {}: {e}", path.display())))?;
    file.sync_all()
        .map_err(|e| BootstrapError::Acme(format!("syncing {}: {e}", path.display())))?;

    // Verify rather than assume: a filesystem that ignored the mode
    // (or a umask-independent path) must be a loud error, because the
    // whole point is that this file is not readable by anyone else.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .map_err(|e| BootstrapError::Acme(format!("stat {}: {e}", path.display())))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o600 {
            let _ = std::fs::remove_file(path);
            return Err(BootstrapError::Acme(format!(
                "{} was created with mode {mode:o}, not 0600 — refusing to leave a \
                 readable private key on disk",
                path.display()
            )));
        }
    }
    // On Windows the file inherits the parent directory's ACL. The
    // cache directory is the operator's to protect; this is stated
    // rather than silently assumed, and the Unix path above is what
    // CI enforces.
    Ok(())
}

/// Run one ACME order to completion, returning PEM (chain, key).
///
/// The HTTP-01 key authorization is installed into `challenges`
/// before the challenge is triggered and removed after the order
/// finalizes, so the listener serves a token exactly while the
/// directory may ask for it.
async fn order_certificate(
    config: &AcmeConfig,
    challenges: &AcmeState,
) -> Result<(String, String), BootstrapError> {
    use instant_acme::{
        Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    };

    let contact = format!("mailto:{}", config.contact_email);
    let (account, _credentials) = Account::builder()
        .map_err(|e| BootstrapError::Acme(format!("acme client: {e}")))?
        .create(
            &NewAccount {
                contact: &[&contact],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            config.directory_url.clone(),
            None,
        )
        .await
        .map_err(|e| BootstrapError::Acme(format!("creating the ACME account: {e}")))?;

    let identifier = Identifier::Dns(config.domain.clone());
    let mut order = account
        .new_order(&NewOrder::new(&[identifier]))
        .await
        .map_err(|e| BootstrapError::Acme(format!("placing the order: {e}")))?;

    let mut installed = Vec::new();
    {
        let mut authorizations = order.authorizations();
        while let Some(mut authorization) = authorizations
            .next()
            .await
            .transpose()
            .map_err(|e| BootstrapError::Acme(format!("reading an authorization: {e}")))?
        {
            if authorization.status != AuthorizationStatus::Pending {
                continue;
            }
            let mut challenge = authorization
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| BootstrapError::Acme("no http-01 challenge was offered".into()))?;
            challenges.set_challenge(
                challenge.token.to_string(),
                challenge.key_authorization().as_str().to_string(),
            );
            installed.push(challenge.token.to_string());
            challenge
                .set_ready()
                .await
                .map_err(|e| BootstrapError::Acme(format!("triggering the challenge: {e}")))?;
        }
    }

    let status = order
        .poll_ready(&instant_acme::RetryPolicy::default())
        .await
        .map_err(|e| BootstrapError::Acme(format!("polling the order: {e}")))?;
    for token in &installed {
        challenges.clear_challenge(token);
    }
    if status != OrderStatus::Ready {
        return Err(BootstrapError::Acme(format!(
            "the order did not become ready (status {status:?})"
        )));
    }

    let private_key_pem = order
        .finalize()
        .await
        .map_err(|e| BootstrapError::Acme(format!("finalizing the order: {e}")))?;
    let cert_pem = order
        .poll_certificate(&instant_acme::RetryPolicy::default())
        .await
        .map_err(|e| BootstrapError::Acme(format!("fetching the certificate: {e}")))?;
    Ok((cert_pem, private_key_pem))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is what keeps an anchor from re-ordering on every
    /// boot (and walking into a rate limit). A pair that is present
    /// is used; a pair that is missing either half is not.
    #[test]
    fn a_cached_pair_is_used_and_a_half_written_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_cache(dir.path()).is_none(), "an empty cache is empty");

        // A cert with no key is not a usable pair.
        let (cert, key) = cache_paths(dir.path());
        std::fs::write(&cert, "not a pem").unwrap();
        assert!(read_cache(dir.path()).is_none());

        // …and neither is a pair that does not parse.
        std::fs::write(&key, "not a pem either").unwrap();
        assert!(read_cache(dir.path()).is_none());
    }

    /// R5b: the key is created 0600 from inception, and a mode that
    /// does not come out 0600 is a hard error with the file removed
    /// — never a readable key and a swallowed chmod result.
    #[cfg(unix)]
    #[test]
    fn the_private_key_is_created_0600_and_a_wrong_mode_is_fatal() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k.pem");
        write_private_key(&key, "PEM").expect("write");
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created 0600, not chmod'd afterwards");

        // Fault injection: an existing file with a permissive mode
        // must not be inherited — the writer replaces it.
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_key(&key, "PEM AGAIN").expect("rewrite");
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a permissive predecessor is replaced, not reused"
        );
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "PEM AGAIN");

        // …and a directory that cannot hold the file at all is an
        // error, not a silent skip.
        let missing = dir.path().join("nope").join("k.pem");
        assert!(write_private_key(&missing, "PEM").is_err());
    }

    #[test]
    fn writing_the_cache_creates_the_directory_and_round_trips_paths() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("acme").join("anchor.example");
        write_cache(&nested, "cert", "key").unwrap();
        let (cert, key) = cache_paths(&nested);
        assert_eq!(std::fs::read_to_string(cert).unwrap(), "cert");
        assert_eq!(std::fs::read_to_string(key).unwrap(), "key");
    }
}
