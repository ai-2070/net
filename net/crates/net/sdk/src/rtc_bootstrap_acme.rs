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
use std::time::Duration;

use crate::rtc_bootstrap::{read_pem_pair, AcmeConfig, AcmeState, BootstrapError};

/// File names inside the **per-domain** cache directory (R4b).
///
/// The cache used to be one flat pair in `cache_dir`, so asking for
/// `different.example` returned the `localhost` certificate that
/// happened to be there. The directory is now
/// `<cache_dir>/<domain>/`, and even then the pair is only reused
/// after it is qualified: it must actually cover the domain and
/// still be inside its validity window.
const CERT_FILE: &str = "bootstrap-cert.pem";
const KEY_FILE: &str = "bootstrap-key.pem";

/// Renew this long before `not_after` (R4c). Let's Encrypt issues
/// 90-day certificates and recommends renewing at 30 days left.
/// Default renewal horizon (R4c).
pub const DEFAULT_RENEWAL_HORIZON: Duration = Duration::from_secs(30 * 24 * 3600);

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
    let dir = domain_cache_dir(config);
    if let Some(cached) = read_cache(&dir, &config.domain, now_unix()) {
        return Ok(cached);
    }
    let (cert_pem, key_pem) = order_certificate(config, challenges).await?;
    write_cache(&dir, &cert_pem, &key_pem)?;
    read_cache(&dir, &config.domain, now_unix()).ok_or_else(|| {
        BootstrapError::Acme(format!(
            "the freshly issued certificate does not qualify for {} — it was written \
             but does not cover the domain, or is already outside its validity window",
            config.domain
        ))
    })
}

/// Seconds since the epoch.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The cache directory for THIS domain (R4b).
pub(crate) fn domain_cache_dir(config: &AcmeConfig) -> PathBuf {
    // A domain is a DNS name: letters, digits, `-`, `.`. Anything
    // else is replaced rather than trusted as a path component.
    let safe: String = config
        .domain
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    config.cache_dir.join(safe)
}

fn cache_paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join(CERT_FILE), dir.join(KEY_FILE))
}

/// Read a cached pair, **qualified** (R4b): it must parse, cover
/// `domain`, and be inside its validity window at `now`.
fn read_cache(dir: &Path, domain: &str, now: u64) -> Option<Chain> {
    let (cert, key) = cache_paths(dir);
    if !cert.exists() || !key.exists() {
        return None;
    }
    let (chain, private) = read_pem_pair(&cert, &key).ok()?;
    let leaf = chain.first()?;
    let (covers, not_after) = qualify_leaf(leaf, domain)?;
    if !covers || not_after <= now {
        return None;
    }
    Some((chain, private))
}

/// `(covers the domain, not_after)` for a leaf certificate.
fn qualify_leaf(leaf: &rustls::pki_types::CertificateDer<'_>, domain: &str) -> Option<(bool, u64)> {
    use x509_parser::prelude::*;

    let (_, parsed) = X509Certificate::from_der(leaf.as_ref()).ok()?;
    let not_after = parsed.validity().not_after.timestamp().max(0) as u64;
    let mut covers = parsed
        .subject()
        .iter_common_name()
        .filter_map(|cn| cn.as_str().ok())
        .any(|cn| name_matches(cn, domain));
    if let Ok(Some(san)) = parsed.subject_alternative_name() {
        covers |= san.value.general_names.iter().any(|name| match name {
            GeneralName::DNSName(dns) => name_matches(dns, domain),
            _ => false,
        });
    }
    Some((covers, not_after))
}

/// Exact match, plus the one wildcard form a certificate may carry.
fn name_matches(name: &str, domain: &str) -> bool {
    if name.eq_ignore_ascii_case(domain) {
        return true;
    }
    match name.strip_prefix("*.") {
        Some(suffix) => domain
            .split_once('.')
            .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(suffix)),
        None => false,
    }
}

/// When the cached certificate for `config` should be renewed (R4c):
/// `not_after - horizon`, or `None` when there is nothing cached.
pub(crate) fn renew_at(config: &AcmeConfig, horizon: Duration) -> Option<u64> {
    let (cert, key) = cache_paths(&domain_cache_dir(config));
    let (chain, _) = read_pem_pair(&cert, &key).ok()?;
    let (_, not_after) = qualify_leaf(chain.first()?, &config.domain)?;
    Some(not_after.saturating_sub(horizon.as_secs()))
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

/// Force a fresh order, ignoring the cache, and write it (R4c).
pub(crate) async fn renew_certificate(
    config: &AcmeConfig,
    challenges: &AcmeState,
) -> Result<Chain, BootstrapError> {
    let dir = domain_cache_dir(config);
    let (cert_pem, key_pem) = order_certificate(config, challenges).await?;
    write_cache(&dir, &cert_pem, &key_pem)?;
    read_cache(&dir, &config.domain, now_unix()).ok_or_else(|| {
        BootstrapError::Acme(format!(
            "the renewed certificate does not qualify for {}",
            config.domain
        ))
    })
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
        assert!(
            read_cache(dir.path(), "localhost", now_unix()).is_none(),
            "an empty cache is empty"
        );

        // A cert with no key is not a usable pair.
        let (cert, key) = cache_paths(dir.path());
        std::fs::write(&cert, "not a pem").unwrap();
        assert!(read_cache(dir.path(), "localhost", now_unix()).is_none());

        // …and neither is a pair that does not parse.
        std::fs::write(&key, "not a pem either").unwrap();
        assert!(read_cache(dir.path(), "localhost", now_unix()).is_none());
    }

    /// R5b: the key is created 0600 from inception, and a mode that
    /// does not come out 0600 is a hard error with the file removed
    /// — never a readable key and a swallowed chmod result.
    /// R4b: a cached pair is only reused when it actually covers the
    /// domain asked for and is still inside its validity window.
    ///
    /// The review asked for `different.example` and got the
    /// `localhost` certificate that happened to be in the directory.
    #[test]
    fn a_cached_pair_is_reused_only_when_it_qualifies() {
        let dir = tempfile::tempdir().unwrap();
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 3600);
        std::fs::write(dir.path().join(CERT_FILE), &cert_pem).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), &key_pem).unwrap();

        let now = now_unix();
        assert!(
            read_cache(dir.path(), "localhost", now).is_some(),
            "the domain it was issued for is reused"
        );
        assert!(
            read_cache(dir.path(), "different.example", now).is_none(),
            "a certificate for another name must never be served for this one"
        );
        assert!(
            read_cache(dir.path(), "localhost", now + 7200).is_none(),
            "an expired pair is not a usable pair"
        );
    }

    /// R4b: the cache is keyed by domain, so two domains cannot
    /// collide in one directory at all.
    #[test]
    fn the_cache_directory_is_per_domain() {
        let base = tempfile::tempdir().unwrap();
        let one = AcmeConfig::new(
            "http://d/",
            "a.example",
            "o@example.invalid",
            base.path().into(),
        );
        let two = AcmeConfig::new(
            "http://d/",
            "b.example",
            "o@example.invalid",
            base.path().into(),
        );
        assert_ne!(domain_cache_dir(&one), domain_cache_dir(&two));
        assert!(domain_cache_dir(&one).ends_with("a.example"));
        // A domain that is not a plain DNS name cannot escape the
        // parent directory.
        let evil = AcmeConfig::new(
            "http://d/",
            "../../etc",
            "o@example.invalid",
            base.path().into(),
        );
        assert_eq!(domain_cache_dir(&evil), base.path().join(".._.._etc"));
    }

    /// R4c: the renewal owner wakes at `not_after - horizon`.
    #[test]
    fn the_renewal_horizon_is_read_from_the_cached_certificate() {
        let base = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new(
            "http://d/",
            "localhost",
            "o@example.invalid",
            base.path().into(),
        );
        let dir = domain_cache_dir(&config);
        std::fs::create_dir_all(&dir).unwrap();
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 90 * 24 * 3600);
        std::fs::write(dir.join(CERT_FILE), &cert_pem).unwrap();
        std::fs::write(dir.join(KEY_FILE), &key_pem).unwrap();

        let horizon = Duration::from_secs(30 * 24 * 3600);
        let at = renew_at(&config, horizon).expect("a cached certificate");
        let now = now_unix();
        // ~60 days out, i.e. 90 - 30, with generous slack for the
        // issuing clock.
        assert!(
            at > now + 55 * 24 * 3600 && at < now + 65 * 24 * 3600,
            "renewal is scheduled at not_after - horizon (in {} days)",
            (at.saturating_sub(now)) / 86_400
        );
        // Nothing cached for a domain means "renew now".
        let cold = AcmeConfig::new(
            "http://d/",
            "cold.example",
            "o@example.invalid",
            base.path().into(),
        );
        assert!(renew_at(&cold, horizon).is_none());
    }

    /// A self-signed leaf for `name`, valid for `ttl` seconds.
    fn issue_for(name: &str, ttl: u64) -> (rcgen::Certificate, String, String) {
        let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        let not_after = std::time::SystemTime::now() + Duration::from_secs(ttl);
        let secs = not_after
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        params.not_after = time::OffsetDateTime::from_unix_timestamp(secs).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let pem = cert.pem();
        (cert, pem, key.serialize_pem())
    }

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
