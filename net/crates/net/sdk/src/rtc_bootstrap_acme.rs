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
//! **So is the ACME account.** Every order used to register a new
//! account and throw its credentials away, so an anchor that kept
//! failing before its first certificate (a crash loop under a
//! supervisor, a validation path that did not work yet) registered a
//! new account on every start, and Let's Encrypt refused it after ten
//! in three hours. The account is created once per directory and
//! restored from the cache from then on (`acme-account-<hash>.json`).
//!
//! **HTTP-01 on the same listener means the same socket answers
//! `:443`.** The plan's constraint is that CI must not pass
//! `--ignore-certificate-errors`; that is satisfied by there being
//! no self-signed path at all, here or in the operator path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::rtc_bootstrap::{read_pem_pair, AcmeConfig, AcmeState, BootstrapError};

/// File names inside the **per-domain** cache directory (R4b).
///
/// The cache used to be one flat pair in `cache_dir`, so asking for
/// `different.example` returned the `localhost` certificate that
/// happened to be there. The directory is now
/// `<cache_dir>/directory-<id>/<domain>/` — per issuing directory
/// (TLS-02) and per domain — and even then the pair is only reused
/// after it is qualified: it must actually cover the domain and
/// still be inside its validity window.
const CERT_FILE: &str = "bootstrap-cert.pem";
const KEY_FILE: &str = "bootstrap-key.pem";

/// The ACME account's saved credentials live in `cache_dir` itself —
/// an account belongs to a directory, not to a domain — one file per
/// directory URL (`<prefix><hash>.json`), so a staging account is
/// never presented to production. The file holds the account's
/// private key and is written 0600 like the certificate key.
const ACCOUNT_FILE_PREFIX: &str = "acme-account-";

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
    let dir = prepare_cache(config)?;
    if let Some(cached) = read_cache(&dir, &config.domain, now_unix()) {
        return Ok(cached);
    }
    if adopt_legacy_pair(config, &dir) {
        if let Some(cached) = read_cache(&dir, &config.domain, now_unix()) {
            return Ok(cached);
        }
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

/// A short, stable identity for [`AcmeConfig::directory_url`]: the
/// first eight bytes of its BLAKE3 digest, hex.
///
/// It names both halves of what a directory owns in the cache — the
/// account file and the certificate namespace (TLS-02) — so a
/// staging account and a staging certificate are equally invisible
/// to production.
fn directory_id(config: &AcmeConfig) -> String {
    let digest = blake3::hash(config.directory_url.as_bytes());
    digest.as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The certificate namespace of THIS ACME directory inside
/// `cache_dir` (TLS-02): `<cache_dir>/directory-<id>/`.
///
/// Certificates used to be keyed by domain alone, so an anchor
/// switched from Let's Encrypt staging to production with the same
/// cache found the staging certificate, qualified it (right name,
/// right key, inside its window — everything but the issuer) and
/// served a chain no browser trusts, for the two months until the
/// renewal horizon came round. A certificate belongs to the
/// directory that issued it, as the account already did.
fn directory_cache_dir(config: &AcmeConfig) -> PathBuf {
    config
        .cache_dir
        .join(format!("directory-{}", directory_id(config)))
}

/// The cache directory for THIS domain (R4b), inside the issuing
/// directory's namespace (TLS-02):
/// `<cache_dir>/directory-<id>/<domain>/`.
pub(crate) fn domain_cache_dir(config: &AcmeConfig) -> PathBuf {
    directory_cache_dir(config).join(safe_domain(config))
}

/// The domain as a path component. A domain is a DNS name: letters,
/// digits, `-`, `.`. Anything else is replaced rather than trusted as
/// a path component. A name that is empty or only dots (`.`, `..`) would
/// join as the parent directory itself, or above it, so it is replaced
/// too: the result is always one ordinary component below its parent.
fn safe_domain(config: &AcmeConfig) -> String {
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
    if safe.chars().all(|c| c == '.') {
        // Covers the empty name as well (`all` of nothing is true).
        "_".repeat(safe.len().max(1))
    } else {
        safe
    }
}

/// Where this domain's pair lived before TLS-02 namespaced the cache by
/// issuing directory: `<cache_dir>/<domain>/`.
fn legacy_domain_cache_dir(config: &AcmeConfig) -> PathBuf {
    config.cache_dir.join(safe_domain(config))
}

/// The roots a public ACME directory's certificates are judged against:
/// the platform trust store, the same one browsers and the anchor's other
/// clients use. Empty when it cannot be loaded.
fn platform_roots() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    if !loaded.errors.is_empty() {
        tracing::debug!(errors = ?loaded.errors, "ACME: some platform roots could not be loaded");
    }
    roots.add_parsable_certificates(loaded.certs);
    roots
}

/// Whether `chain` verifies as a TLS server chain for `domain` against
/// `roots`, now — the check every client of the anchor will make.
fn chain_is_trusted(
    chain: &[rustls::pki_types::CertificateDer<'static>],
    domain: &str,
    roots: rustls::RootCertStore,
) -> bool {
    use rustls::client::danger::ServerCertVerifier as _;

    let Some((leaf, intermediates)) = chain.split_first() else {
        return false;
    };
    let Ok(name) = rustls::pki_types::ServerName::try_from(domain.to_string()) else {
        return false;
    };
    let Ok(verifier) = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build() else {
        return false;
    };
    verifier
        .verify_server_cert(
            leaf,
            intermediates,
            &name,
            &[],
            rustls::pki_types::UnixTime::now(),
        )
        .is_ok()
}

/// The TLS-02 upgrade: adopt this domain's pre-TLS-02 pair into the
/// issuing directory's namespace, so an anchor that already holds a good
/// certificate does not place a new order (and spend rate limit) just
/// because the layout changed. Returns whether a pair was adopted.
///
/// The old layout recorded nothing about which directory issued a pair,
/// and nothing in the cache can prove it after the fact (account files
/// can be deleted). So provenance is not inferred; the harm TLS-02 exists
/// for — serving a certificate the anchor's clients will refuse, such as a
/// staging one — is checked directly instead. For a public directory (no
/// [`AcmeConfig::directory_roots`]) the legacy chain is adopted only if it
/// verifies for the domain against the platform trust store, which a
/// staging certificate never does. A private directory adopts nothing: its
/// clients' trust is not known here. The pair must also pass the trust
/// checks the cache itself gets (TLS-06: no symlink, this user's, not
/// writable by others, the key private) and qualify for the domain now. It
/// is copied, not moved: the legacy files stay where an older build looks
/// for them. Anything else adopts nothing and the anchor orders as usual.
fn adopt_legacy_pair(config: &AcmeConfig, dir: &Path) -> bool {
    if !config.directory_roots.is_empty() {
        return false;
    }
    adopt_legacy_pair_trusting(config, dir, platform_roots)
}

/// [`adopt_legacy_pair`] judged against the roots `roots` yields (loaded
/// only once a legacy pair is actually there).
fn adopt_legacy_pair_trusting(
    config: &AcmeConfig,
    dir: &Path,
    roots: impl FnOnce() -> rustls::RootCertStore,
) -> bool {
    let legacy = legacy_domain_cache_dir(config);
    let (cert, key) = cache_paths(&legacy);
    if !cert.exists() || !key.exists() {
        return false;
    }
    for (path, private) in [
        (legacy.clone(), false),
        (cert.clone(), false),
        (key.clone(), true),
    ] {
        if let Err(e) = check_cache_entry(&path, private) {
            tracing::warn!(error = %e, "ACME: not adopting the pre-namespace cached certificate");
            return false;
        }
    }
    let Some((chain, _)) = read_cache(&legacy, &config.domain, now_unix()) else {
        return false;
    };
    if !chain_is_trusted(&chain, &config.domain, roots()) {
        tracing::info!(
            from = %legacy.display(),
            "ACME: not adopting the pre-namespace cached certificate: it does not verify \
             against the platform trust store (it may be a staging certificate)"
        );
        return false;
    }
    let (Ok(cert_pem), Ok(key_pem)) = (
        std::fs::read_to_string(&cert),
        std::fs::read_to_string(&key),
    ) else {
        return false;
    };
    match write_cache(dir, &cert_pem, &key_pem) {
        Ok(()) => {
            tracing::info!(
                from = %legacy.display(),
                to = %dir.display(),
                "ACME: adopted the cached certificate into its directory's namespace"
            );
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "ACME: could not adopt the pre-namespace cached certificate");
            false
        }
    }
}

/// Is `path` safe to trust as part of the cache (TLS-06)?
///
/// `Ok(false)` when it does not exist, `Ok(true)` when it does and
/// passes, an error when it exists and must not be trusted:
///
/// - **a symlink** — anywhere in the cache, a link is a name someone
///   else may point at their own material;
/// - on Unix, **owned by another user** — a pair another local user
///   planted (the default used to be the shared, predictable
///   `/tmp/net-mesh-acme`) would be served as this anchor's
///   certificate;
/// - on Unix, **group- or world-writable** — another user could
///   replace it after this check;
/// - on Unix, for a `private` entry (the TLS key and the ACME account
///   file), **readable by group or other** — another local user may
///   already have read the key this anchor would serve with. The
///   certificate is public and keeps its 0644.
///
/// The key's own 0600 mode (R5b) does not authenticate a
/// pre-existing cache: it is the mode of what THIS process writes,
/// not evidence of who wrote what is already there.
///
/// On Windows there is no ownership/mode view in `std`; only the
/// symlink refusal applies, and the default cache lives under the
/// per-user profile, whose ACL is the user's.
fn check_cache_entry(path: &Path, private: bool) -> Result<bool, BootstrapError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(BootstrapError::Acme(format!(
                "inspecting the ACME cache entry {}: {e}",
                path.display()
            )))
        }
    };
    if meta.file_type().is_symlink() {
        return Err(BootstrapError::Acme(format!(
            "the ACME cache entry {} is a symbolic link — refusing to trust a cache \
             another user may control; point --acme-cache at a private directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` takes no arguments, touches no memory and
        // cannot fail; it is unsafe only because it is `extern`.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(BootstrapError::Acme(format!(
                "the ACME cache entry {} is owned by uid {}, not this user (uid {euid}) — \
                 refusing to serve or extend a cache another user controls",
                path.display(),
                meta.uid()
            )));
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o022 != 0 {
            return Err(BootstrapError::Acme(format!(
                "the ACME cache entry {} has mode {mode:o}, writable by group or other — \
                 refusing a cache another user could replace",
                path.display()
            )));
        }
        if private && mode & 0o077 != 0 {
            return Err(BootstrapError::Acme(format!(
                "the ACME cache entry {} has mode {mode:o}, readable by group or other — \
                 refusing private key material another user may already have read",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(true)
}

/// Every directory ABOVE the cache must not let another user swap the
/// cache out from under the per-entry checks (TLS-06 follow-up): an
/// ancestor another (non-root) user owns, or one others may write without
/// the sticky bit, lets them rename the checked cache directory away and
/// put their own in its place between the check and the read.
///
/// Checked along both the configured path and its canonical form (a
/// symlinked component is judged by its owner — its permission bits mean
/// nothing — and its target's ancestors are checked through the canonical
/// path). An ancestor that does not exist yet is skipped: the cache
/// creates it private. Unix only, like the ownership checks; on Windows
/// the default cache lives under the per-user profile.
#[cfg(unix)]
fn check_cache_ancestors(cache_dir: &Path) -> Result<(), BootstrapError> {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: `geteuid` takes no arguments, touches no memory and
    // cannot fail; it is unsafe only because it is `extern`.
    let euid = unsafe { libc::geteuid() };
    let absolute = std::path::absolute(cache_dir).map_err(|e| {
        BootstrapError::Acme(format!(
            "resolving the ACME cache path {}: {e}",
            cache_dir.display()
        ))
    })?;
    let mut ancestors: Vec<PathBuf> = absolute
        .ancestors()
        .skip(1)
        .map(Path::to_path_buf)
        .collect();
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        ancestors.extend(canonical.ancestors().skip(1).map(Path::to_path_buf));
    }
    for dir in ancestors {
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(BootstrapError::Acme(format!(
                    "inspecting {} above the ACME cache: {e}",
                    dir.display()
                )))
            }
        };
        if meta.uid() != euid && meta.uid() != 0 {
            return Err(BootstrapError::Acme(format!(
                "{} above the ACME cache is owned by uid {}, not this user or root — \
                 its owner could replace the cache; point --acme-cache at a private directory",
                dir.display(),
                meta.uid()
            )));
        }
        let mode = meta.mode();
        if !meta.file_type().is_symlink() && mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(BootstrapError::Acme(format!(
                "{} above the ACME cache has mode {:o}, writable by others without the sticky \
                 bit — they could replace the cache; point --acme-cache at a private directory",
                dir.display(),
                mode & 0o7777
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_cache_ancestors(_cache_dir: &Path) -> Result<(), BootstrapError> {
    Ok(())
}

/// Every level of the cache this config reads or writes, each
/// required to pass [`check_cache_entry`] if it exists (TLS-06):
/// `cache_dir`, the directory namespace, the domain directory, the
/// certificate pair and the account file.
fn check_cache(config: &AcmeConfig) -> Result<(), BootstrapError> {
    let dir = domain_cache_dir(config);
    let (cert, key) = cache_paths(&dir);
    check_cache_ancestors(&config.cache_dir)?;
    for (path, private) in [
        (config.cache_dir.clone(), false),
        (directory_cache_dir(config), false),
        (dir, false),
        (cert, false),
        (key, true),
        (account_path(config), true),
    ] {
        check_cache_entry(&path, private)?;
    }
    Ok(())
}

/// Create the cache's directories private (0700 on Unix) where they
/// do not exist yet, then check every level (TLS-06). Returns the
/// domain directory.
///
/// The check runs AFTER creation, so a directory someone else
/// created first — between this process deciding it was absent and
/// creating it — is still refused.
fn prepare_cache(config: &AcmeConfig) -> Result<PathBuf, BootstrapError> {
    let dir = domain_cache_dir(config);
    create_private_dir_all(&dir)?;
    check_cache(config)?;
    Ok(dir)
}

/// `create_dir_all`, every created level 0700 on Unix (TLS-06).
///
/// An explicit mode rather than the umask's: under a `002` umask
/// (the user-private-group default on many distributions) plain
/// `create_dir_all` makes group-writable directories, which
/// [`check_cache_entry`] would then refuse on the next start.
fn create_private_dir_all(dir: &Path) -> Result<(), BootstrapError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| BootstrapError::Acme(format!("creating {}: {e}", dir.display())))
}

/// Write the (public) certificate chain with an explicit 0644 on
/// Unix, never a umask-derived group-writable mode that
/// [`check_cache_entry`] would refuse (TLS-06).
fn write_public_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o644);
    }
    let mut file = options
        .open(path)
        .map_err(|e| BootstrapError::Acme(format!("creating {}: {e}", path.display())))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| BootstrapError::Acme(format!("writing {}: {e}", path.display())))?;
    file.sync_all()
        .map_err(|e| BootstrapError::Acme(format!("syncing {}: {e}", path.display())))
}

fn cache_paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join(CERT_FILE), dir.join(KEY_FILE))
}

/// Where the account for [`AcmeConfig::directory_url`] is saved.
fn account_path(config: &AcmeConfig) -> PathBuf {
    config.cache_dir.join(format!(
        "{ACCOUNT_FILE_PREFIX}{}.json",
        directory_id(config)
    ))
}

/// The saved account for this directory, if there is one that parses.
/// A file that does not parse is treated as absent: the next order
/// creates an account and replaces it.
fn load_account(config: &AcmeConfig) -> Option<instant_acme::AccountCredentials> {
    let text = std::fs::read_to_string(account_path(config)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Save a newly created account for this directory, 0600.
fn save_account(
    config: &AcmeConfig,
    credentials: &instant_acme::AccountCredentials,
) -> Result<(), BootstrapError> {
    create_private_dir_all(&config.cache_dir)?;
    let json = serde_json::to_string(credentials)
        .map_err(|e| BootstrapError::Acme(format!("encoding the ACME account: {e}")))?;
    write_private_key(&account_path(config), &json)
}

/// Does `private` belong to `leaf`? (R4, round two.)
///
/// The pair on disk is two files published by two renames, so a
/// crash between the renames can leave a NEW half beside an OLD one.
/// The reader must reject such a pair — "the leaf will not verify
/// against the key" was claimed but never checked, so the torn pair
/// was served and bricked every subsequent start. The public key the
/// private key derives must equal the leaf's `subjectPublicKeyInfo`
/// bits; a key that cannot be parsed answers conservatively (`false`)
/// — an unverifiable pair is a rejected pair.
fn key_matches_leaf(
    leaf: &rustls::pki_types::CertificateDer<'_>,
    private: &rustls::pki_types::PrivateKeyDer<'_>,
) -> bool {
    use x509_parser::prelude::*;
    let Ok((_, parsed)) = X509Certificate::from_der(leaf.as_ref()) else {
        return false;
    };
    let Ok(pair) = rcgen::KeyPair::try_from(private) else {
        return false;
    };
    parsed.public_key().subject_public_key.data == pair.public_key_raw()
}

/// Read the cached pair, requiring the two halves to MATCH.
fn read_pair_checked(cert: &PathBuf, key: &PathBuf) -> Option<Chain> {
    let (chain, private) = read_pem_pair(cert, key).ok()?;
    if !key_matches_leaf(chain.first()?, &private) {
        return None;
    }
    Some((chain, private))
}

/// Read a cached pair, **qualified** (R4b): its two halves must
/// match each other, it must parse, cover `domain`, and be inside
/// its validity window at `now`.
fn read_cache(dir: &Path, domain: &str, now: u64) -> Option<Chain> {
    let (cert, key) = cache_paths(dir);
    if !cert.exists() || !key.exists() {
        return None;
    }
    let (chain, private) = read_pair_checked(&cert, &key)?;
    let leaf = chain.first()?;
    let (covers, not_before, not_after) = qualify_leaf(leaf, domain)?;
    // **The WHOLE window** (R4, round two): a leaf whose validity
    // starts in the future was being served — the review cached one
    // and watched it accepted. Every verifying client would refuse
    // it, so serving it is an outage with a cache hit in front of
    // it.
    if !covers || not_after <= now || not_before > now {
        return None;
    }
    Some((chain, private))
}

/// `(covers the domain, not_before, not_after)` for a leaf.
///
/// **SAN is authoritative** (R4, round two). The previous form OR'd
/// Common Name with the SAN, so a matching CN overrode a
/// non-matching DNS SAN — the review cached exactly that pair and
/// watched it accepted. Every browser and every rustls client has
/// ignored CN for hostname verification for years: a certificate
/// that carries a SAN is covered by its SAN or not at all. CN is
/// consulted only for a certificate with no SAN extension, which is
/// the legacy shape those clients also refuse but which this cache
/// has no business inventing an opinion about.
fn qualify_leaf(
    leaf: &rustls::pki_types::CertificateDer<'_>,
    domain: &str,
) -> Option<(bool, u64, u64)> {
    use x509_parser::prelude::*;

    let (_, parsed) = X509Certificate::from_der(leaf.as_ref()).ok()?;
    let not_after = parsed.validity().not_after.timestamp().max(0) as u64;
    let not_before = parsed.validity().not_before.timestamp().max(0) as u64;
    let covers = match parsed.subject_alternative_name() {
        Ok(Some(san)) => san.value.general_names.iter().any(|name| match name {
            GeneralName::DNSName(dns) => name_matches(dns, domain),
            _ => false,
        }),
        _ => parsed
            .subject()
            .iter_common_name()
            .filter_map(|cn| cn.as_str().ok())
            .any(|cn| name_matches(cn, domain)),
    };
    Some((covers, not_before, not_after))
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
    // A cache that fails its ownership check has nothing usable in it
    // (TLS-06): "renew now", and the renewal itself refuses the cache
    // loudly.
    check_cache(config).ok()?;
    let (cert, key) = cache_paths(&domain_cache_dir(config));
    let (chain, _private) = read_pair_checked(&cert, &key)?;
    let (_, _, not_after) = qualify_leaf(chain.first()?, &config.domain)?;
    Some(not_after.saturating_sub(horizon.as_secs()))
}

/// Publish a new pair, **key first** (R4, round two).
///
/// The old shape wrote the certificate over the live one and then
/// deleted the key to recreate it: an I/O failure in between left a
/// new certificate beside a deleted key, and the process could not
/// restart on either. Both halves are now written to siblings first
/// — the key created 0600 from inception, as before — and only when
/// BOTH are on disk is either renamed into place, so an I/O failure
/// during the writes leaves the previous pair intact.
///
/// Two files cannot be renamed in one step, so a CRASH between the
/// renames still tears the pair — which is why [`read_cache`]
/// verifies the key against the leaf and REJECTS a torn pair (the
/// self-healing the previous doc claimed without implementing), so
/// the next order replaces it. The KEY goes first so the one
/// unrecoverable tear — a published certificate whose matching key
/// was then deleted, the old cert-first order's error arm — cannot
/// occur; on a failed second rename the unpublished certificate
/// (`cert_tmp`) is kept, being the only file that still matches the
/// published key.
fn write_cache(dir: &Path, cert_pem: &str, key_pem: &str) -> Result<(), BootstrapError> {
    create_private_dir_all(dir)?;
    let (cert, key) = cache_paths(dir);
    let cert_tmp = cert.with_extension("pem.new");
    let key_tmp = key.with_extension("pem.new");
    write_public_file(&cert_tmp, cert_pem)?;
    write_private_key(&key_tmp, key_pem)?;
    std::fs::rename(&key_tmp, &key).map_err(|e| {
        // Nothing was published: the old pair is intact, and the
        // half-written siblings go away.
        let _ = std::fs::remove_file(&cert_tmp);
        let _ = std::fs::remove_file(&key_tmp);
        BootstrapError::Acme(format!("publishing {}: {e}", key.display()))
    })?;
    std::fs::rename(&cert_tmp, &cert).map_err(|e| {
        // The key half is already published and the old pair is
        // torn. `cert_tmp` is KEPT — it matches the published key.
        BootstrapError::Acme(format!("publishing {}: {e}", cert.display()))
    })?;
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
    write_private_key_verified(path, pem, observed_mode)
}

/// The observed mode of a file, as the protection check reads it.
#[cfg(unix)]
fn observed_mode(path: &Path) -> Result<u32, BootstrapError> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(std::fs::metadata(path)
        .map_err(|e| BootstrapError::Acme(format!("stat {}: {e}", path.display())))?
        .permissions()
        .mode()
        & 0o777)
}

/// On Windows the file inherits the parent directory's ACL; there is
/// no mode to read, and the check has nothing to assert.
#[cfg(not(unix))]
fn observed_mode(_path: &Path) -> Result<u32, BootstrapError> {
    Ok(0o600)
}

/// [`write_private_key`] with the protection observation injectable,
/// so the FAILURE branch can be exercised (R5, round two).
///
/// The review's point: the existing test covered normal creation, a
/// permissive predecessor and an unopenable parent — never a created
/// file that fails the check, so removing the verification would
/// have kept every assertion green. A filesystem that ignores mode
/// bits cannot be conjured in a unit test; the observation can.
fn write_private_key_verified(
    path: &Path,
    pem: &str,
    observe: fn(&Path) -> Result<u32, BootstrapError>,
) -> Result<(), BootstrapError> {
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
    let mode = observe(path)?;
    if mode != 0o600 {
        let _ = std::fs::remove_file(path);
        return Err(BootstrapError::Acme(format!(
            "{} was created with mode {mode:o}, not 0600 — refusing to leave a \
             readable private key on disk",
            path.display()
        )));
    }
    Ok(())
}

/// Force a fresh order, ignoring the cache, and write it (R4c).
pub(crate) async fn renew_certificate(
    config: &AcmeConfig,
    challenges: &AcmeState,
) -> Result<Chain, BootstrapError> {
    let dir = prepare_cache(config)?;
    let (cert_pem, key_pem) = order_certificate(config, challenges).await?;
    write_cache(&dir, &cert_pem, &key_pem)?;
    read_cache(&dir, &config.domain, now_unix()).ok_or_else(|| {
        BootstrapError::Acme(format!(
            "the renewed certificate does not qualify for {}",
            config.domain
        ))
    })
}

/// The ACME account builder, with the directory connection's trust
/// anchored where [`AcmeConfig::directory_roots`] says.
///
/// Empty list — the overwhelmingly common case, Let's Encrypt —
/// means [`instant_acme::Account::builder`], i.e. the platform trust
/// store, unchanged.
///
/// Non-empty means a private ACME CA, and the connection to the
/// directory is verified against exactly those roots. Note what is
/// NOT used here: `Account::builder_with_root` exists in
/// instant-acme 0.8 and would be one line, but it accepts a single
/// certificate from a single file, and it reaches
/// `rustls::ClientConfig::builder()`, which installs a
/// **process-global** crypto provider. This crate's rule — the same
/// one `server_config` follows and `net-payments` documents — is an
/// explicit provider, never a global, because a global leaks into
/// every other rustls user sharing the process. So the HTTP client
/// is built here and handed over through
/// `Account::builder_with_http`.
fn account_builder(config: &AcmeConfig) -> Result<instant_acme::AccountBuilder, BootstrapError> {
    if config.directory_roots.is_empty() {
        return instant_acme::Account::builder()
            .map_err(|e| BootstrapError::Acme(format!("acme client: {e}")));
    }

    let mut roots = rustls::RootCertStore::empty();
    for path in &config.directory_roots {
        let pem = std::fs::read(path).map_err(|e| {
            BootstrapError::Acme(format!("reading directory root {}: {e}", path.display()))
        })?;
        let mut found = 0usize;
        for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
            let cert = cert.map_err(|e| {
                BootstrapError::Acme(format!("parsing directory root {}: {e}", path.display()))
            })?;
            roots.add(cert).map_err(|e| {
                BootstrapError::Acme(format!("trusting directory root {}: {e}", path.display()))
            })?;
            found += 1;
        }
        // A silently-empty trust store would make every directory
        // connection fail with an opaque handshake error; say which
        // file was empty instead.
        if found == 0 {
            return Err(BootstrapError::Acme(format!(
                "directory root {} contains no certificate",
                path.display()
            )));
        }
    }

    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| BootstrapError::Acme(format!("acme directory tls: {e}")))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        // `https_only`: the directory URL is HTTPS, and a private CA
        // is no reason to let it be downgraded.
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
    let http = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build::<_, instant_acme::BodyWrapper<bytes::Bytes>>(connector);
    Ok(instant_acme::Account::builder_with_http(Box::new(http)))
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
        AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    };

    // The saved account when there is one. A restore that fails (the
    // directory unreachable, say) fails the order: falling back to a
    // fresh registration is exactly the churn this cache exists to
    // stop. An operator retires a dead account by deleting its file.
    let account = match load_account(config) {
        Some(credentials) => account_builder(config)?
            .from_credentials(credentials)
            .await
            .map_err(|e| BootstrapError::Acme(format!("restoring the ACME account: {e}")))?,
        None => {
            let contact = format!("mailto:{}", config.contact_email);
            let (account, credentials) = account_builder(config)?
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
            // Saving can fail after the account exists. Carry on: this
            // order still completes and caches its certificate, so a
            // restart does not order again, where failing here would
            // hand a supervisor another registration to make.
            if let Err(e) = save_account(config, &credentials) {
                tracing::warn!(
                    error = %e,
                    "the new ACME account could not be saved; the next order will \
                     register another"
                );
            }
            account
        }
    };

    let identifier = Identifier::Dns(config.domain.clone());
    let mut order = account
        .new_order(&NewOrder::new(&[identifier]))
        .await
        .map_err(|e| BootstrapError::Acme(format!("placing the order: {e}")))?;

    // **Every exit retires this order's challenges** (R4, round
    // two). `set_ready` and `poll_ready` can both fail, and the
    // early return used to leave key authorizations installed on the
    // public challenge route for the life of the process.
    struct Installed<'a> {
        challenges: &'a AcmeState,
        tokens: Vec<String>,
    }
    impl Drop for Installed<'_> {
        fn drop(&mut self) {
            for token in &self.tokens {
                self.challenges.clear_challenge(token);
            }
        }
    }
    let mut installed = Installed {
        challenges,
        tokens: Vec::new(),
    };
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
            installed.tokens.push(challenge.token.to_string());
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
    drop(installed);
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
        assert_eq!(
            domain_cache_dir(&evil),
            directory_cache_dir(&evil).join(".._.._etc")
        );
        assert!(domain_cache_dir(&evil).starts_with(base.path()));
    }

    /// TLS-02: a certificate cached for one ACME directory is never
    /// served for another. The scenario: an anchor is brought up
    /// against Let's Encrypt STAGING, then pointed at production with
    /// the same domain and the same `--acme-cache`. The staging
    /// certificate is a perfectly good pair — right name, matching
    /// key, inside its window — and browsers refuse its issuer.
    ///
    /// Inverse: key the certificate cache on the domain alone (the
    /// previous shape) and production finds the staging pair.
    #[test]
    fn a_certificate_from_another_acme_directory_is_not_reused() {
        let base = tempfile::tempdir().unwrap();
        let staging = AcmeConfig::new(
            "https://acme-staging-v02.api.letsencrypt.org/directory",
            "localhost",
            "o@example.invalid",
            base.path().into(),
        );
        let production = AcmeConfig::new(
            "https://acme-v02.api.letsencrypt.org/directory",
            "localhost",
            "o@example.invalid",
            base.path().into(),
        );
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 90 * 24 * 3600);
        write_cache(&domain_cache_dir(&staging), &cert_pem, &key_pem).unwrap();

        let now = now_unix();
        assert!(
            read_cache(&domain_cache_dir(&staging), "localhost", now).is_some(),
            "the premise: staging's own cache hit is served"
        );
        assert_ne!(domain_cache_dir(&staging), domain_cache_dir(&production));
        assert!(
            read_cache(&domain_cache_dir(&production), "localhost", now).is_none(),
            "production must not serve the staging certificate"
        );
        assert!(
            renew_at(&production, DEFAULT_RENEWAL_HORIZON).is_none(),
            "production has nothing cached, so it orders now instead of \
             sleeping out the staging certificate's renewal horizon"
        );
        assert!(renew_at(&staging, DEFAULT_RENEWAL_HORIZON).is_some());
    }

    /// A domain that is empty or only dots never names the cache directory
    /// itself or anything above it: neither the namespaced nor the
    /// pre-namespace (legacy) domain directory may leave `cache_dir`, so
    /// the legacy adoption can never inspect or copy files outside it.
    ///
    /// Inverse: `..` resolves the legacy domain directory to the cache's
    /// parent.
    #[test]
    fn a_dot_segment_domain_stays_one_component_below_the_cache() {
        let base = tempfile::tempdir().unwrap();
        for domain in ["", ".", "..", "...", "a.b", "x/../y"] {
            let config = AcmeConfig::new(
                "https://acme-v02.api.letsencrypt.org/directory",
                domain,
                "o@example.invalid",
                base.path().into(),
            );
            for (dir, parent) in [
                (legacy_domain_cache_dir(&config), config.cache_dir.clone()),
                (domain_cache_dir(&config), directory_cache_dir(&config)),
            ] {
                assert_eq!(dir.parent(), Some(parent.as_path()), "{domain:?}");
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                let name = name.unwrap_or_default();
                assert!(
                    !name.is_empty() && !name.chars().all(|c| c == '.'),
                    "{domain:?} became the component {name:?}"
                );
            }
        }
    }

    /// A CA and a `localhost` leaf it signed: `(root store, leaf PEM, key PEM)`.
    fn ca_signed_localhost() -> (rustls::RootCertStore, String, String) {
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "net-mesh adoption test CA");
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.clone().self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&leaf_key, &issuer)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        (roots, leaf.pem(), leaf_key.serialize_pem())
    }

    /// TLS-02 upgrade: a pair cached under the pre-namespace layout
    /// (`<cache>/<domain>/`) is adopted when its chain verifies for the
    /// domain against the trust the anchor's clients use, so an upgraded
    /// anchor does not re-order a certificate it already holds. A chain
    /// that does not verify — a staging certificate, which no client
    /// trusts — is never adopted, whatever account files the cache holds:
    /// deleting the staging account (the case an account-based check got
    /// wrong) changes nothing. A private directory adopts nothing.
    ///
    /// Inverse: adopt by account files alone, and a staging pair whose
    /// account was deleted is served in production.
    #[test]
    fn a_pre_namespace_pair_is_adopted_only_when_its_chain_is_trusted() {
        let production = |base: &Path| {
            AcmeConfig::new(
                "https://acme-v02.api.letsencrypt.org/directory",
                "localhost",
                "o@example.invalid",
                base.into(),
            )
        };
        let now = now_unix();

        // A trusted chain is adopted, and copied rather than moved.
        let base = tempfile::tempdir().unwrap();
        let config = production(base.path());
        let (roots, cert_pem, key_pem) = ca_signed_localhost();
        write_cache(&legacy_domain_cache_dir(&config), &cert_pem, &key_pem).unwrap();
        let dir = prepare_cache(&config).unwrap();
        assert!(read_cache(&dir, "localhost", now).is_none(), "the premise");
        assert!(adopt_legacy_pair_trusting(&config, &dir, || roots));
        assert!(read_cache(&dir, "localhost", now).is_some());
        assert!(renew_at(&config, DEFAULT_RENEWAL_HORIZON).is_some());
        assert!(
            read_cache(&legacy_domain_cache_dir(&config), "localhost", now).is_some(),
            "copied, not moved: an older build still finds it"
        );

        // The reviewed case: a staging pair (its CA is trusted by no
        // client) with the staging account deleted and only production's
        // account left. Not adopted, against the platform store or against
        // roots that do not include its CA.
        let base = tempfile::tempdir().unwrap();
        let config = production(base.path());
        let (_staging_roots, cert_pem, key_pem) = ca_signed_localhost();
        write_cache(&legacy_domain_cache_dir(&config), &cert_pem, &key_pem).unwrap();
        write_private_key(&account_path(&config), "{}").unwrap();
        let dir = prepare_cache(&config).unwrap();
        let (other_roots, _, _) = ca_signed_localhost();
        assert!(
            !adopt_legacy_pair_trusting(&config, &dir, || other_roots),
            "a chain no trusted root signed is not adopted"
        );
        assert!(
            !adopt_legacy_pair(&config, &dir),
            "the platform store does not trust it either"
        );
        assert!(read_cache(&dir, "localhost", now).is_none());

        // A self-signed pair (no chain at all) is not adopted.
        let base = tempfile::tempdir().unwrap();
        let config = production(base.path());
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 90 * 24 * 3600);
        write_cache(&legacy_domain_cache_dir(&config), &cert_pem, &key_pem).unwrap();
        let dir = prepare_cache(&config).unwrap();
        assert!(!adopt_legacy_pair(&config, &dir));

        // A private directory adopts nothing, even a chain its own roots
        // would verify: its clients' trust is not known here.
        let base = tempfile::tempdir().unwrap();
        let root_file = base.path().join("root.pem");
        let (_roots, cert_pem, key_pem) = ca_signed_localhost();
        std::fs::write(&root_file, &cert_pem).unwrap();
        let config = production(base.path()).with_directory_roots([root_file]);
        write_cache(&legacy_domain_cache_dir(&config), &cert_pem, &key_pem).unwrap();
        let dir = prepare_cache(&config).unwrap();
        assert!(!adopt_legacy_pair(&config, &dir));
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
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 90 * 24 * 3600);
        // Through the writer, so the cache's modes are the ones the
        // anchor produces, not the test process's umask (TLS-06).
        write_cache(&dir, &cert_pem, &key_pem).unwrap();

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

    /// TLS-06: the cache is created private and a clean one passes
    /// its own check, so the guard does not refuse what this process
    /// itself writes.
    #[test]
    fn a_fresh_cache_is_created_private_and_passes_its_check() {
        let base = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new(
            "http://d/",
            "localhost",
            "o@example.invalid",
            base.path().join("nested").join("acme"),
        );
        let dir = prepare_cache(&config).expect("a fresh cache is prepared");
        assert_eq!(dir, domain_cache_dir(&config));
        assert!(dir.is_dir());
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 3600);
        write_cache(&dir, &cert_pem, &key_pem).unwrap();
        save_account(&config, &credentials()).unwrap();
        check_cache(&config).expect("what this process wrote is trusted");
        assert!(renew_at(&config, DEFAULT_RENEWAL_HORIZON).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for level in [config.cache_dir.clone(), directory_cache_dir(&config), dir] {
                let mode = std::fs::metadata(&level).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{} is created private", level.display());
            }
        }
    }

    /// TLS-06: a cache another local user could have planted or can
    /// still replace is refused, not served.
    ///
    /// The scenario: the default cache was the predictable shared
    /// `/tmp/net-mesh-acme`, and another user pre-created it holding
    /// a matching key and a self-signed certificate with the right
    /// SAN. Startup reused it, skipped issuance, and served a chain
    /// every browser refuses. A different uid cannot be conjured
    /// without root, so this drives the two conditions an unprivileged
    /// test can create — a group/world-writable level and a symlinked
    /// one — through the same check a foreign owner fails.
    ///
    /// Inverse: drop `check_cache` from `prepare_cache` / `renew_at`
    /// and every refusal below becomes a cache hit.
    #[cfg(unix)]
    #[test]
    fn a_cache_another_user_could_control_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new(
            "http://d/",
            "localhost",
            "o@example.invalid",
            base.path().join("acme"),
        );
        let dir = prepare_cache(&config).unwrap();
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 3600);
        write_cache(&dir, &cert_pem, &key_pem).unwrap();
        assert!(
            prepare_cache(&config).is_ok(),
            "the premise: a private cache"
        );

        // A world-writable cache root.
        std::fs::set_permissions(&config.cache_dir, std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a world-writable cache directory is refused"
        );
        assert!(
            renew_at(&config, DEFAULT_RENEWAL_HORIZON).is_none(),
            "…and renewal does not schedule off it"
        );
        std::fs::set_permissions(&config.cache_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();

        // A group-writable certificate.
        let (cert, _key) = cache_paths(&dir);
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a group-writable cached certificate is refused"
        );
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(prepare_cache(&config).is_ok());

        // A domain directory that is a symlink to someone else's pair.
        let elsewhere = base.path().join("planted");
        std::fs::rename(&dir, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a symlinked domain directory is refused"
        );
        assert!(renew_at(&config, DEFAULT_RENEWAL_HORIZON).is_none());
    }

    /// TLS-06 follow-up: a private key (or ACME account file) other users
    /// can READ is refused, though only writability was checked before; the
    /// public certificate keeps its 0644.
    #[cfg(unix)]
    #[test]
    fn a_cached_key_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new(
            "http://d/",
            "localhost",
            "o@example.invalid",
            base.path().join("acme"),
        );
        let dir = prepare_cache(&config).unwrap();
        let (_ca, cert_pem, key_pem) = issue_for("localhost", 3600);
        write_cache(&dir, &cert_pem, &key_pem).unwrap();
        assert!(
            prepare_cache(&config).is_ok(),
            "the premise: a private cache"
        );

        let (_cert, key) = cache_paths(&dir);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a world-readable private key is refused"
        );
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a group-readable private key is refused"
        );
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(prepare_cache(&config).is_ok());
    }

    /// TLS-06 follow-up: a directory ABOVE the cache that others may write
    /// (without the sticky bit) lets them swap the checked cache for their
    /// own, so it is refused; with the sticky bit it is fine (`/tmp`).
    #[cfg(unix)]
    #[test]
    fn a_cache_under_a_directory_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = tempfile::tempdir().unwrap();
        let shared = base.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        let config = AcmeConfig::new(
            "http://d/",
            "localhost",
            "o@example.invalid",
            shared.join("acme"),
        );
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            prepare_cache(&config).is_err(),
            "a world-writable, non-sticky parent is refused"
        );
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(
            prepare_cache(&config).is_ok(),
            "a sticky shared parent cannot have its entries swapped by others"
        );
    }

    /// R4 (round two): a leaf whose validity has not STARTED is not
    /// a usable pair.
    ///
    /// The review cached one and watched the helper accept it. Every
    /// verifying client refuses a not-yet-valid certificate, so
    /// serving it is an outage with a cache hit in front of it.
    #[test]
    fn a_certificate_that_is_not_valid_yet_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_unix();
        let (_ca, cert_pem, key_pem) = issue_between("localhost", now + 3600, now + 7200);
        std::fs::write(dir.path().join(CERT_FILE), &cert_pem).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), &key_pem).unwrap();
        assert!(
            read_cache(dir.path(), "localhost", now).is_none(),
            "a leaf whose not_before is in the future must not be served",
        );
        // The positive control: the same pair once its window opens.
        assert!(
            read_cache(dir.path(), "localhost", now + 4000).is_some(),
            "…and it IS usable inside its window",
        );
    }

    /// R4 (round two): the SAN is authoritative for the hostname.
    ///
    /// A matching Common Name used to override a non-matching DNS
    /// SAN, so the cache served a certificate every browser and
    /// every rustls client would refuse for that name.
    #[test]
    fn a_matching_common_name_cannot_override_a_wrong_san() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_unix();
        let (_ca, cert_pem, key_pem) = issue_with_cn("wanted.example", "other.example");
        std::fs::write(dir.path().join(CERT_FILE), &cert_pem).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), &key_pem).unwrap();
        assert!(
            read_cache(dir.path(), "wanted.example", now).is_none(),
            "CN says yes, the SAN says no, and the SAN is what clients check",
        );
        assert!(
            read_cache(dir.path(), "other.example", now).is_some(),
            "the name it is genuinely for is still served",
        );
    }

    /// R4 (round two): publishing a new pair never destroys a usable
    /// old one.
    #[test]
    fn a_failed_publication_leaves_the_previous_pair_intact() {
        let dir = tempfile::tempdir().unwrap();
        let (_ca, old_cert, old_key) = issue_for("localhost", 3600);
        write_cache(dir.path(), &old_cert, &old_key).expect("first publication");
        let before = read_cache(dir.path(), "localhost", now_unix());
        assert!(before.is_some(), "the premise: a usable cached pair");

        // A key path that cannot be created: publication must fail
        // BEFORE anything live is touched.
        let (cert, key) = cache_paths(dir.path());
        let blocker = key.with_extension("pem.new");
        std::fs::create_dir_all(&blocker).expect("occupy the key's staging path");
        let (_ca2, new_cert, new_key) = issue_for("localhost", 7200);
        assert!(
            write_cache(dir.path(), &new_cert, &new_key).is_err(),
            "the premise: this publication fails",
        );
        assert!(
            read_cache(dir.path(), "localhost", now_unix()).is_some(),
            "the previous pair must still be usable after a failed publication",
        );
        assert_eq!(
            std::fs::read_to_string(&cert).unwrap(),
            old_cert,
            "…and must still be the OLD certificate",
        );
    }

    /// R5 (round two): the protection CHECK is what is under test.
    ///
    /// Removing the verification used to leave every other assertion
    /// green, because nothing ever made a created file fail it. Here
    /// the observation reports a permissive mode: the write must be
    /// a hard error naming the mode, and — the part that matters —
    /// the readable key must not be left on disk. Runs on every
    /// platform, because the injected observation is the fault, not
    /// the filesystem.
    #[test]
    fn a_key_that_fails_its_protection_check_is_an_error_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k.pem");
        let err = write_private_key_verified(&key, "PEM", |_| Ok(0o644))
            .expect_err("a key observed as 0644 must not be accepted");
        assert!(
            err.to_string().contains("644") && err.to_string().contains("0600"),
            "the error must name what it saw and what it required: {err}",
        );
        assert!(
            !key.exists(),
            "a key that failed its protection check must not survive the failure",
        );
        // The positive control: the same path with a conforming
        // observation writes the file.
        write_private_key_verified(&key, "PEM", |_| Ok(0o600)).expect("write");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "PEM");
    }

    /// R4, round two: the cached pair must MATCH. A crash between
    /// the two publication renames leaves a mixed pair on disk, and
    /// the reader must reject it in either direction — that rejection
    /// is the "next order replaces" self-healing the doc claims. The
    /// matching pair is the positive control. Inverse: the pre-fix
    /// `read_cache` never compared key to leaf and served the torn
    /// pair, bricking every subsequent start.
    #[test]
    fn a_torn_pair_is_rejected_in_either_direction_and_a_matching_pair_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (_, cert_a, key_a) = issue_for("pair-check.example", 3600);
        let (_, cert_b, key_b) = issue_for("pair-check.example", 3600);

        // Control: the pair as issued is accepted.
        write_cache(dir.path(), &cert_a, &key_a).expect("publish");
        assert!(
            read_cache(dir.path(), "pair-check.example", now_unix()).is_some(),
            "a pair whose halves match must be served"
        );

        // The cert-first tear (the old publication order): a new
        // certificate beside the old key.
        write_cache(dir.path(), &cert_b, &key_a).expect("publish torn");
        assert!(
            read_cache(dir.path(), "pair-check.example", now_unix()).is_none(),
            "a certificate that does not verify against the cached key is a \
             torn pair and must be rejected, not served"
        );

        // The key-first tear (the new publication order): a new key
        // beside the old certificate.
        write_cache(dir.path(), &cert_a, &key_b).expect("publish torn");
        assert!(
            read_cache(dir.path(), "pair-check.example", now_unix()).is_none(),
            "the other tear shape is rejected the same way"
        );
    }

    /// A self-signed leaf for `name`, valid over an explicit window.
    fn issue_between(
        name: &str,
        not_before: u64,
        not_after: u64,
    ) -> (rcgen::Certificate, String, String) {
        let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        params.not_before = time::OffsetDateTime::from_unix_timestamp(not_before as i64).unwrap();
        params.not_after = time::OffsetDateTime::from_unix_timestamp(not_after as i64).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let pem = cert.pem();
        let key_pem = key.serialize_pem();
        (cert, pem, key_pem)
    }

    /// A leaf whose Common Name and DNS SAN name different hosts.
    fn issue_with_cn(common_name: &str, san: &str) -> (rcgen::Certificate, String, String) {
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        let secs = (now_unix() + 3600) as i64;
        params.not_after = time::OffsetDateTime::from_unix_timestamp(secs).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let pem = cert.pem();
        let key_pem = key.serialize_pem();
        (cert, pem, key_pem)
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

    /// R4, hosted closure: the directory-trust seam loads real roots,
    /// names a bad root file instead of failing later as an opaque
    /// handshake error, and — the rule this module shares with
    /// `server_config` — does NOT install a process-global crypto
    /// provider on the way.
    ///
    /// That last point is why `Account::builder_with_root` is not
    /// used: it reaches `rustls::ClientConfig::builder()`, which
    /// installs one from crate features, and it would take exactly
    /// one certificate from exactly one file.
    #[test]
    fn directory_roots_are_loaded_without_a_global_crypto_provider() {
        let dir = tempfile::tempdir().unwrap();
        let base = AcmeConfig::new(
            "https://directory.example/dir",
            "anchor.example",
            "o@example.invalid",
            dir.path().into(),
        );

        let root = dir.path().join("ca.pem");
        let (_ca, ca_pem, _key) = issue_for("directory.example", 3600);
        std::fs::write(&root, &ca_pem).unwrap();
        // Two roots in one store, and more than one certificate in a
        // file: the CA-rotation shape a single-root API cannot
        // express.
        let pair = dir.path().join("both.pem");
        let (_ca2, ca2_pem, _key2) = issue_for("directory.other", 3600);
        std::fs::write(&pair, format!("{ca_pem}{ca2_pem}")).unwrap();

        let had_global = rustls::crypto::CryptoProvider::get_default().is_some();
        assert!(
            account_builder(&base.clone().with_directory_roots([root, pair])).is_ok(),
            "several roots, and several certificates per file, are accepted"
        );
        if !had_global {
            assert!(
                rustls::crypto::CryptoProvider::get_default().is_none(),
                "building the ACME directory client must not install a process-global \
                 crypto provider — it would leak into every other rustls user here"
            );
        }

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "").unwrap();
        let Err(err) = account_builder(&base.clone().with_directory_roots([empty])) else {
            panic!("a root file with no certificate in it is not usable trust");
        };
        let err = err.to_string();
        assert!(err.contains("contains no certificate"), "{err}");

        let missing = dir.path().join("absent.pem");
        let Err(err) = account_builder(&base.with_directory_roots([missing])) else {
            panic!("a root file that is not there is an error");
        };
        assert!(err.to_string().contains("reading directory root"), "{err}");
    }

    /// A CA, and a certificate it signed for `127.0.0.1`.
    ///
    /// An IP SAN rather than a name so the test never depends on how
    /// this machine resolves `localhost`.
    fn issue_ca_and_loopback_leaf() -> (String, String, String) {
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "net-mesh directory test CA");
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.clone().self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        let leaf_params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
        (
            ca.pem(),
            format!("{}{}", leaf.pem(), ca.pem()),
            leaf_key.serialize_pem(),
        )
    }

    fn staging(cache: &Path) -> AcmeConfig {
        AcmeConfig::new(
            "https://acme-staging-v02.api.letsencrypt.org/directory",
            "anchor.example",
            "o@example.invalid",
            cache.to_path_buf(),
        )
    }

    /// Credentials as instant-acme serializes them. The type is opaque
    /// and only deserializes; the key bytes are never used here.
    fn credentials() -> instant_acme::AccountCredentials {
        serde_json::from_value(serde_json::json!({
            "id": "https://acme.example/acct/1",
            "key_pkcs8": "MFECAQEwBQYDK2VwBCIEIA",
            "directory": "https://acme.example/directory",
        }))
        .expect("credentials parse")
    }

    /// A saved account comes back for the same directory, is private
    /// on disk, and is never offered to a different directory: a
    /// staging account must not be presented to production.
    ///
    /// Inverse: key the file on nothing (one name for every directory)
    /// and the production lookup finds the staging account.
    #[test]
    fn an_account_is_saved_per_directory_and_restored() {
        let cache = tempfile::tempdir().unwrap();
        let config = staging(cache.path());
        assert!(
            load_account(&config).is_none(),
            "an empty cache has no account"
        );

        save_account(&config, &credentials()).expect("save");
        let back = load_account(&config).expect("the saved account loads");
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(credentials()).unwrap()
        );
        let path = account_path(&config);
        assert!(path.starts_with(cache.path()));
        assert_eq!(
            observed_mode(&path).unwrap(),
            0o600,
            "the account key is private"
        );

        let production = AcmeConfig::new(
            "https://acme-v02.api.letsencrypt.org/directory",
            "anchor.example",
            "o@example.invalid",
            cache.path().to_path_buf(),
        );
        assert_ne!(account_path(&production), path);
        assert!(
            load_account(&production).is_none(),
            "another directory's account is not reused"
        );
    }

    /// A file that does not parse is no account, so the next order
    /// registers one and overwrites it, rather than failing forever.
    #[test]
    fn an_unreadable_account_file_counts_as_none() {
        let cache = tempfile::tempdir().unwrap();
        let config = staging(cache.path());
        std::fs::write(account_path(&config), "not json").unwrap();
        assert!(load_account(&config).is_none());
        save_account(&config, &credentials()).expect("replaces the junk");
        assert!(load_account(&config).is_some());
    }

    /// **The failure this closes.** The hosted cold-start job died at
    /// `creating the ACME account: client error (Connect)` while the
    /// directory logged `remote error: tls: unknown certificate
    /// authority` — the ACME client refused the directory's
    /// certificate. This drives a real TLS handshake from the real
    /// client `account_builder` produces and observes the SERVER side
    /// of it: a root in `directory_roots` completes the handshake, an
    /// unrelated root still refuses it.
    ///
    /// The negative half is the part that matters: the fix trusts one
    /// more CA, it does not stop verifying.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_directory_root_decides_whether_the_handshake_completes() {
        let dir = tempfile::tempdir().unwrap();
        let (ca_pem, chain_pem, key_pem) = issue_ca_and_loopback_leaf();
        let (unrelated_pem, _, _) = issue_ca_and_loopback_leaf();
        let good = dir.path().join("directory-ca.pem");
        let wrong = dir.path().join("someone-elses-ca.pem");
        std::fs::write(&good, &ca_pem).unwrap();
        std::fs::write(&wrong, &unrelated_pem).unwrap();

        let chain: Vec<_> = rustls_pemfile::certs(&mut chain_pem.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .unwrap()
            .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<bool>();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(acceptor.accept(stream).await.is_ok());
                });
            }
        });

        let url = format!("https://127.0.0.1:{port}/dir");
        // The server answers nothing, so the ACME call always fails;
        // WHERE it fails is the point, and the server says where.
        async fn attempt(cache: &Path, roots: PathBuf, url: String) {
            let config = AcmeConfig::new(
                url.clone(),
                "anchor.example",
                "o@example.invalid",
                cache.to_path_buf(),
            )
            .with_directory_roots([roots]);
            let account = instant_acme::NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            };
            let Ok(builder) = account_builder(&config) else {
                panic!("the builder accepts a real root file");
            };
            let _ =
                tokio::time::timeout(Duration::from_secs(20), builder.create(&account, url, None))
                    .await;
        }

        attempt(dir.path(), good, url.clone()).await;
        assert!(
            rx.recv().await.unwrap(),
            "a CA named in `directory_roots` completes the handshake to the directory"
        );

        attempt(dir.path(), wrong, url).await;
        assert!(
            !rx.recv().await.unwrap(),
            "an unrelated CA is still refused — the seam adds a trust anchor, it does \
             not stop verifying"
        );
    }
}
