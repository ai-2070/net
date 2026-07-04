//! Caller-side forwarding **policy** store + audit surface
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 1).
//!
//! This is the persistent home of the caller daemon's `forwarding:` policy —
//! the kill switch, the secret refs and their destination bindings, and the
//! plain-header rules. It is deliberately a **policy** store, not a value
//! store: it records *that* `github-token` may go to `node-1` as
//! `Authorization` for `github.*`, never the token itself. Secret values enter
//! through a separate value backend (OS keychain / encrypted store) that isn't
//! built yet — this store holds only what is safe to audit, so
//! [`ForwardingStore::audit`] can list every grant without ever touching a
//! secret.
//!
//! Persistence mirrors the pin store ([`crate::serve`]'s `PinStore`): a
//! per-user JSON file, atomic temp-and-rename writes, owner-only (0600 on
//! Unix), and every read-modify-write under a cross-process advisory lock so a
//! concurrent `net secret` invocation can't clobber a change. A missing file
//! is an empty, **disabled** policy (forwarding off) — the safe first-run
//! state; a present-but-unparseable file is an error, never a silent reset to
//! "forward nothing" that hides a corrupted allowlist.
//!
//! Mutations validate at write time, so the store can never hold a policy the
//! decision path would have to reject later: a non-forwardable (hop-by-hop)
//! header, a cookie without the explicit override, a security-sensitive header
//! masquerading as a plain one, or a ref name shaped like a secret value.

use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::header::{HeaderError, HeaderName};
use super::policy::{
    AllowList, ForwardingConfig, PlainHeaderPolicy, PolicyError, ProviderScope, SecretPolicy,
};

/// Max length of a secret ref name. Names are audit-legible labels, not values.
const MAX_REF_NAME_LEN: usize = 64;

/// Current on-disk schema version. Bumped only on a breaking format change.
const SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// A failure loading, saving, or editing the forwarding policy store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An I/O error reading or writing the store file.
    #[error("forwarding store I/O error at {path}: {reason}")]
    Io {
        /// The file path involved.
        path: String,
        /// The stringified underlying I/O error.
        reason: String,
    },
    /// The store file exists but does not parse.
    #[error("forwarding store at {path} is corrupt: {reason}")]
    Corrupt {
        /// The file path involved.
        path: String,
        /// Why it failed to parse.
        reason: String,
    },
    /// A secret ref name was empty, too long, or outside the slug charset.
    #[error("invalid secret ref name {name:?}: {reason}")]
    InvalidRefName {
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The configured wire header can never be forwarded (hop-by-hop).
    #[error("header {name:?} cannot be forwarded: {reason}")]
    HeaderNotForwardable {
        /// The offending header name.
        name: String,
        /// Why it can't be forwarded.
        reason: &'static str,
    },
    /// `cookie` / `set-cookie` was configured without the explicit override.
    #[error("configuring {name:?} requires the explicit cookie override")]
    CookieRequiresForce {
        /// The offending header name.
        name: String,
    },
    /// A security-sensitive header was configured as a *plain* (non-secret)
    /// header — `plain_headers` is for trace / tenant ids, not credentials.
    #[error("security-sensitive header {name:?} cannot be a plain header; use a secret")]
    SensitiveHeaderNotPlain {
        /// The offending header name.
        name: String,
    },
    /// A secret was bound to *any* provider — secrets must name specific ones.
    #[error(
        "secret ref {ref_name:?} allows any provider; secrets must be bound to specific providers"
    )]
    SecretProviderAny {
        /// The offending ref name.
        ref_name: String,
    },
    /// A header name could not be canonicalized.
    #[error(transparent)]
    Header(#[from] HeaderError),
}

impl StoreError {
    fn io(path: &Path, e: std::io::Error) -> Self {
        StoreError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        }
    }
}

// A policy-validation failure (shared write-time / load-time rules) maps onto
// the store's error surface so both `set_*` and `load` report the same reasons.
impl From<PolicyError> for StoreError {
    fn from(e: PolicyError) -> Self {
        match e {
            PolicyError::InvalidHeader { name } => StoreError::HeaderNotForwardable {
                name,
                reason: "not a valid header name",
            },
            PolicyError::HopByHop { name } => StoreError::HeaderNotForwardable {
                name,
                reason: "hop-by-hop headers are never forwarded",
            },
            PolicyError::CookieNotAcknowledged { name } => StoreError::CookieRequiresForce { name },
            PolicyError::SecretProviderAny { ref_name } => {
                StoreError::SecretProviderAny { ref_name }
            }
            PolicyError::SensitiveAsPlain { name } => StoreError::SensitiveHeaderNotPlain { name },
        }
    }
}

/// Holds the cross-process advisory lock on the store's `.lock` sidecar for the
/// lifetime of a [`ForwardingStore::mutate`] transaction. Mirrors the pin
/// store's guard — kept local so the forwarding module takes no dependency on
/// `serve` internals. Dropping it releases the OS lock.
struct LockGuard {
    _file: std::fs::File,
}

impl LockGuard {
    async fn acquire(store_path: &Path) -> Result<Self, StoreError> {
        let lock_path = PathBuf::from(format!("{}.lock", store_path.display()));
        let display = lock_path.clone();
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            if let Some(parent) = lock_path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)?;
            file.lock_exclusive()?;
            Ok(file)
        })
        .await
        .map_err(|e| StoreError::Io {
            path: display.display().to_string(),
            reason: format!("forwarding-store lock task panicked: {e}"),
        })?
        .map_err(|e| StoreError::io(&display, e))?;
        Ok(Self { _file: file })
    }
}

// On-disk shape: a versioned wrapper around the policy so a future schema bump
// is not a breaking format change. `deny_unknown_fields` so a typo'd top-level
// field (e.g. a mis-spelled security key) fails closed rather than being
// silently ignored — matching the inner `ForwardingConfig`. A genuine future
// format adds fields under a bumped `schema_version`, which the load-time
// version check already rejects for this build.
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ForwardingFile {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    forwarding: ForwardingConfig,
}

/// The persistent, machine-shared caller forwarding policy.
#[derive(Debug, Clone)]
pub struct ForwardingStore {
    path: PathBuf,
    config: ForwardingConfig,
}

impl ForwardingStore {
    /// Load the store at `path`. A missing file is an empty, **disabled**
    /// policy (forwarding off) — the safe first-run default. A present but
    /// unparseable file is [`StoreError::Corrupt`], never a silent reset.
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let corrupt = |reason: String| StoreError::Corrupt {
                    path: path.display().to_string(),
                    reason,
                };
                let file: ForwardingFile =
                    serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
                // Fail closed on an unrecognized (e.g. newer) schema rather than
                // misreading a future format as the current one.
                if file.schema_version != SCHEMA_VERSION {
                    return Err(corrupt(format!(
                        "unsupported schema version {} (expected {SCHEMA_VERSION})",
                        file.schema_version
                    )));
                }
                // Revalidate every persisted policy with the *same* rules the
                // write-time mutators enforce, so a config hand-edited into a
                // forbidden state (a value-shaped or otherwise malformed ref
                // name, a cookie secret without the acknowledgement, a secret
                // bound to any provider, a hop-by-hop or credential plain header)
                // is rejected on load instead of silently becoming active. Store
                // safety checks can't be bypassed by editing the JSON directly.
                for (ref_name, policy) in &file.forwarding.secrets {
                    // The ref-name charset/length rule is a write-time guard
                    // (`set_secret`), so it must be re-enforced here too — a
                    // hand-edited config keyed by a raw token would otherwise
                    // load active and leak that token through `audit()`.
                    validate_ref_name(ref_name).map_err(|e| corrupt(e.to_string()))?;
                    policy
                        .validate(ref_name)
                        .map_err(|e| corrupt(e.to_string()))?;
                }
                for (name, policy) in &file.forwarding.plain_headers {
                    policy.validate(name).map_err(|e| corrupt(e.to_string()))?;
                }
                Ok(Self {
                    path,
                    config: file.forwarding,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path,
                config: ForwardingConfig::default(),
            }),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    /// Persist the store atomically (temp write + rename), owner-only on Unix.
    pub async fn save(&self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| StoreError::io(&self.path, e))?;
            }
        }

        let file = ForwardingFile {
            schema_version: SCHEMA_VERSION,
            forwarding: self.config.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| StoreError::Io {
            path: self.path.display().to_string(),
            reason: format!("serialize forwarding store: {e}"),
        })?;

        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));

        // Owner-only from creation (0600 on Unix) — the store records
        // security-sensitive destination bindings and must never be briefly
        // group-/world-readable. The mode travels with the inode through the
        // atomic rename. (Windows scopes access via the per-user data dir ACL.)
        use tokio::io::AsyncWriteExt;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // `tokio::fs::OpenOptions::mode` is an inherent unix method — no
        // `OpenOptionsExt` import needed (mirrors the pin store).
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp).await.map_err(|e| StoreError::io(&tmp, e))?;
        // From here the temp file exists, so any failure must not leave it
        // behind (it holds a partial policy and clutters the per-user data dir).
        // Do the write + fsync + rename in a fallible block and remove the temp
        // on error.
        let write_result = async {
            f.write_all(&bytes)
                .await
                .map_err(|e| StoreError::io(&tmp, e))?;
            // Durability: flush the userspace buffer, then fsync the file's data
            // + metadata. Atomic rename gives *atomicity* (a reader sees the old
            // or the new file, never a torn one), but not *durability* — without
            // sync_all a crash after the rename can surface a truncated /
            // zero-length store, which load() rejects as Corrupt and bricks every
            // `net forwarding` verb until the file is deleted by hand.
            f.flush().await.map_err(|e| StoreError::io(&tmp, e))?;
            f.sync_all().await.map_err(|e| StoreError::io(&tmp, e))?;
            drop(f);

            tokio::fs::rename(&tmp, &self.path)
                .await
                .map_err(|e| StoreError::io(&self.path, e))
        }
        .await;
        if let Err(e) = write_result {
            let _ = tokio::fs::remove_file(&tmp).await; // best-effort cleanup
            return Err(e);
        }

        // fsync the parent directory so the new dirent (the rename itself)
        // survives a crash too. Best-effort and unix-only: Windows has no
        // portable directory fsync, and a failure here doesn't invalidate the
        // already-renamed file.
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Ok(dir) = tokio::fs::File::open(parent).await {
                    let _ = dir.sync_all().await;
                }
            }
        }
        Ok(())
    }

    /// Apply a mutation under a cross-process exclusive lock (load → apply →
    /// save). If the closure returns `Err`, nothing is saved — and because the
    /// edit methods validate before mutating, a rejected edit leaves the store
    /// unchanged. Read-only [`load`](Self::load) needs no lock.
    pub async fn mutate<R, F>(path: impl Into<PathBuf>, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&mut ForwardingStore) -> Result<R, StoreError>,
    {
        let path = path.into();
        let _guard = LockGuard::acquire(&path).await?;
        let mut store = ForwardingStore::load(&path).await?;
        let result = f(&mut store)?;
        store.save().await?;
        Ok(result)
    }

    /// The store's file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current policy (read-only). Feed it to the [`ForwardingConfig`]
    /// decision methods (`decide_secret` / `decide_plain`).
    pub fn config(&self) -> &ForwardingConfig {
        &self.config
    }

    /// Whether forwarding is enabled (the global kill switch).
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Flip the global kill switch.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
    }

    /// Add or replace a secret ref: the wire `header` it injects as, the
    /// `allow` binding, and an optional audit `purpose`. Validates before
    /// touching the config, so a rejected edit changes nothing.
    ///
    /// `force` is required to configure `cookie` / `set-cookie` (session
    /// cookies are ambient authority in its worst form). Hop-by-hop headers are
    /// refused unconditionally.
    pub fn set_secret(
        &mut self,
        ref_name: &str,
        header: &str,
        allow: AllowList,
        purpose: Option<String>,
        force: bool,
    ) -> Result<(), StoreError> {
        validate_ref_name(ref_name)?;
        let canon = HeaderName::parse(header)?;
        let is_cookie = matches!(canon.as_str(), "cookie" | "set-cookie");
        let policy = SecretPolicy {
            header: canon.as_str().to_string(),
            allow,
            purpose,
            // `force` records the cookie acknowledgement — only meaningful for
            // cookie headers, ignored otherwise.
            allow_cookie: is_cookie && force,
        };
        // The same rules the store revalidates on load: forwardable header,
        // cookie acknowledgement, and no `providers: any` for a secret.
        policy.validate(ref_name)?;
        self.config.secrets.insert(ref_name.to_string(), policy);
        Ok(())
    }

    /// Remove a secret ref. Returns whether a ref was removed.
    pub fn remove_secret(&mut self, ref_name: &str) -> bool {
        self.config.secrets.remove(ref_name).is_some()
    }

    /// Add or replace a plain (non-secret) header rule. The header must **not**
    /// be security-sensitive — `plain_headers` carries trace / tenant ids, not
    /// credentials. Stored under the canonical header name.
    pub fn set_plain_header(&mut self, header: &str, allow: AllowList) -> Result<(), StoreError> {
        let canon = HeaderName::parse(header)?;
        let policy = PlainHeaderPolicy { allow };
        // End-to-end + non-credential — the same rules as load-time revalidation.
        policy.validate(canon.as_str())?;
        self.config
            .plain_headers
            .insert(canon.as_str().to_string(), policy);
        Ok(())
    }

    /// Remove a plain-header rule (by any casing). Returns whether one was
    /// removed.
    pub fn remove_plain_header(&mut self, header: &str) -> bool {
        let Ok(target) = HeaderName::parse(header) else {
            return false;
        };
        // Config keys are stored canonical, but tolerate a legacy raw key too.
        let key = self
            .config
            .plain_headers
            .keys()
            .find(|k| HeaderName::parse(k).ok().as_ref() == Some(&target))
            .cloned();
        match key {
            Some(k) => self.config.plain_headers.remove(&k).is_some(),
            None => false,
        }
    }

    /// A redaction-safe audit of every active grant — the data behind
    /// `net security audit`. The store holds no values, so this is safe by
    /// construction; every field here is a name, a scope, or a label.
    pub fn audit(&self) -> ForwardingAudit {
        let secret_grants = self
            .config
            .secrets
            .iter()
            .map(|(ref_name, p)| Grant {
                kind: GrantKind::Secret,
                ref_name: ref_name.clone(),
                header: p.header.clone(),
                providers: render_providers(&p.allow.providers),
                capabilities: p.allow.capabilities.clone(),
                purpose: p.purpose.clone(),
            })
            .collect();
        let plain_grants = self
            .config
            .plain_headers
            .iter()
            .map(|(name, p)| Grant {
                kind: GrantKind::Plain,
                ref_name: name.clone(),
                header: name.clone(),
                providers: render_providers(&p.allow.providers),
                capabilities: p.allow.capabilities.clone(),
                purpose: None,
            })
            .collect();
        ForwardingAudit {
            enabled: self.config.enabled,
            secret_grants,
            plain_grants,
        }
    }
}

/// Whether a grant is a secret credential or a plain (non-secret) header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// A secret header (a credential the value store holds).
    Secret,
    /// A non-secret header (trace / tenant id).
    Plain,
}

/// One active forwarding grant, value-free — safe to print in `net security
/// audit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    /// Whether this is a secret or plain-header grant.
    pub kind: GrantKind,
    /// The user-visible ref name (secret) or header name (plain).
    pub ref_name: String,
    /// The wire header the value is injected as.
    pub header: String,
    /// Rendered provider scope (`any`, `(none)`, or a comma-joined id list).
    pub providers: String,
    /// Capability-id globs this grant covers.
    pub capabilities: Vec<String>,
    /// Optional audit-legibility label.
    pub purpose: Option<String>,
}

/// The whole forwarding policy, redaction-safe, as a structured audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForwardingAudit {
    /// Whether the global kill switch is on.
    pub enabled: bool,
    /// Secret grants (credentials).
    pub secret_grants: Vec<Grant>,
    /// Plain-header grants (non-secret).
    pub plain_grants: Vec<Grant>,
}

impl ForwardingAudit {
    /// Whether any grant is configured at all.
    pub fn is_empty(&self) -> bool {
        self.secret_grants.is_empty() && self.plain_grants.is_empty()
    }

    /// A human-readable, value-free rendering for the CLI. Leads with the kill
    /// switch, since a disabled switch means none of the grants below are live.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "forwarding: {}",
            if self.enabled { "ENABLED" } else { "disabled" }
        );
        if !self.enabled {
            let _ = writeln!(out, "  (kill switch off — no grant below is live)");
        }
        if self.is_empty() {
            let _ = writeln!(out, "  no grants configured");
            return out;
        }
        for g in self.secret_grants.iter().chain(self.plain_grants.iter()) {
            let kind = match g.kind {
                GrantKind::Secret => "secret",
                GrantKind::Plain => "plain ",
            };
            let caps = if g.capabilities.is_empty() {
                "(none)".to_string()
            } else {
                g.capabilities.join(",")
            };
            let purpose = g
                .purpose
                .as_deref()
                .map(|p| format!("  # {p}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  [{kind}] {ref_name} -> {header}  providers={providers}  capabilities={caps}{purpose}",
                ref_name = g.ref_name,
                header = g.header,
                providers = g.providers,
            );
        }
        out
    }
}

/// Render a provider scope for audit display (never a value — scopes are ids).
fn render_providers(scope: &ProviderScope) -> String {
    match scope {
        ProviderScope::None => "(none)".to_string(),
        ProviderScope::Any => "any".to_string(),
        ProviderScope::Ids(ids) if ids.is_empty() => "(none)".to_string(),
        ProviderScope::Ids(ids) => ids.join(","),
    }
}

/// A secret ref name is an audit-legible **label**, so it must be a lowercase
/// slug: `[a-z0-9]` first, then `[a-z0-9._-]`, up to [`MAX_REF_NAME_LEN`]. The
/// lowercase-only rule is also a light guard against pasting a raw token as the
/// name (real tokens usually carry uppercase) — the plan's "never encode a
/// value in the name" is a convention the charset can only partly enforce, so
/// the value-in-name prohibition stays a documented convention on top.
///
/// Public so the value-entry path (`net forwarding set-value`) can reject a
/// mistyped ref name at entry — a keychain value stored under a name the policy
/// side would never accept as a slug can never be resolved, and would otherwise
/// fail silently as `ValueMissing` at forward time.
pub fn validate_ref_name(name: &str) -> Result<(), StoreError> {
    let reject = |reason| {
        Err(StoreError::InvalidRefName {
            name: name.to_string(),
            reason,
        })
    };
    if name.is_empty() {
        return reject("empty");
    }
    if name.len() > MAX_REF_NAME_LEN {
        return reject("too long (max 64)");
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !matches!(first, 'a'..='z' | '0'..='9') {
        return reject("must start with a lowercase letter or digit");
    }
    if !name
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
    {
        return reject("only lowercase [a-z0-9._-] allowed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::DenialLevel;

    fn allow(providers: ProviderScope, caps: &[&str]) -> AllowList {
        AllowList {
            providers,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn store_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("forwarding.json");
        (dir, path)
    }

    #[tokio::test]
    async fn missing_file_is_disabled_and_empty() {
        let (_dir, path) = store_path();
        let store = ForwardingStore::load(&path).await.unwrap();
        assert!(!store.is_enabled(), "first run is off");
        assert!(store.audit().is_empty());
    }

    #[tokio::test]
    async fn set_secret_persists_and_decides() {
        let (_dir, path) = store_path();
        ForwardingStore::mutate(path.clone(), |s| {
            s.set_enabled(true);
            s.set_secret(
                "github-token",
                "Authorization",
                allow(ProviderScope::Ids(vec!["node-1".into()]), &["github.*"]),
                Some("github-api".into()),
                false,
            )
        })
        .await
        .unwrap();

        // A fresh load sees it, and the persisted policy decides correctly.
        let store = ForwardingStore::load(&path).await.unwrap();
        assert!(store.is_enabled());
        let grant = store
            .config()
            .decide_secret("github-token", "node-1", "github.issues")
            .unwrap();
        assert_eq!(grant.header.as_str(), "authorization");
        assert_eq!(
            store
                .config()
                .decide_secret("github-token", "node-evil", "github.issues"),
            Err(DenialLevel::PerIdentity),
        );
    }

    #[tokio::test]
    async fn set_secret_rejects_hop_by_hop_and_gates_cookie() {
        let (_dir, path) = store_path();
        let mut store = ForwardingStore::load(&path).await.unwrap();
        assert!(matches!(
            store
                .set_secret("x", "Connection", AllowList::default(), None, false)
                .unwrap_err(),
            StoreError::HeaderNotForwardable { .. },
        ));
        assert!(matches!(
            store
                .set_secret("sess", "Cookie", AllowList::default(), None, false)
                .unwrap_err(),
            StoreError::CookieRequiresForce { .. },
        ));
        // With force, the cookie secret is accepted and stored canonicalized.
        store
            .set_secret("sess", "Cookie", AllowList::default(), None, true)
            .unwrap();
        assert_eq!(store.config().secrets["sess"].header, "cookie");
        // Nothing partial was stored for the rejected edits.
        assert!(!store.config().secrets.contains_key("x"));
    }

    #[tokio::test]
    async fn invalid_ref_names_are_rejected() {
        let (_dir, path) = store_path();
        let mut store = ForwardingStore::load(&path).await.unwrap();
        for bad in [
            "",
            "Github-Token",
            "has space",
            "-leading",
            "ghpUPPER",
            &"x".repeat(65),
        ] {
            assert!(
                matches!(
                    store.set_secret(bad, "Authorization", AllowList::default(), None, false),
                    Err(StoreError::InvalidRefName { .. }),
                ),
                "{bad:?} should be rejected",
            );
        }
        // A clean slug is fine.
        assert!(store
            .set_secret(
                "prod-github.token_1",
                "Authorization",
                AllowList::default(),
                None,
                false
            )
            .is_ok());
    }

    #[test]
    fn public_validate_ref_name_gates_the_value_entry_path() {
        // `net forwarding set-value` calls this to reject a mistyped ref name at
        // entry; lock the public contract the CLI depends on.
        assert!(validate_ref_name("github-token").is_ok());
        for bad in ["Github-Token", "", "has space", "-leading", &"x".repeat(65)] {
            assert!(
                matches!(
                    validate_ref_name(bad),
                    Err(StoreError::InvalidRefName { .. })
                ),
                "{bad:?} must be rejected",
            );
        }
    }

    #[tokio::test]
    async fn plain_header_cannot_be_sensitive() {
        let (_dir, path) = store_path();
        let mut store = ForwardingStore::load(&path).await.unwrap();
        assert!(matches!(
            store
                .set_plain_header("Authorization", AllowList::default())
                .unwrap_err(),
            StoreError::SensitiveHeaderNotPlain { .. },
        ));
        // A trace header is fine and stored canonicalized.
        store
            .set_plain_header("X-Trace-Id", allow(ProviderScope::Any, &["*"]))
            .unwrap();
        assert!(store.config().plain_headers.contains_key("x-trace-id"));
    }

    #[tokio::test]
    async fn remove_secret_and_plain_header() {
        let (_dir, path) = store_path();
        let mut store = ForwardingStore::load(&path).await.unwrap();
        store
            .set_secret("t", "Authorization", AllowList::default(), None, false)
            .unwrap();
        store
            .set_plain_header("X-Trace-Id", AllowList::default())
            .unwrap();
        assert!(store.remove_secret("t"));
        assert!(!store.remove_secret("t"), "second remove is a no-op");
        // Plain header removable by a different casing than stored.
        assert!(store.remove_plain_header("x-TRACE-id"));
        assert!(!store.remove_plain_header("x-trace-id"));
    }

    #[tokio::test]
    async fn corrupt_file_is_an_error_not_a_silent_reset() {
        let (_dir, path) = store_path();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"{ not valid json").await.unwrap();
        let err = ForwardingStore::load(&path).await.unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }));
    }

    /// Write `json` to a temp store file and attempt to load it.
    async fn load_json(json: serde_json::Value) -> Result<(), StoreError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forwarding.json");
        tokio::fs::write(&path, serde_json::to_vec(&json).unwrap())
            .await
            .unwrap();
        // `dir` is dropped only after the load completes.
        ForwardingStore::load(&path).await.map(|_| ())
    }

    #[tokio::test]
    async fn load_revalidates_persisted_policies() {
        // A config hand-edited into a forbidden state must be rejected on load,
        // not silently used — the store's safety checks can't be bypassed by
        // editing the JSON directly.

        // secret bound to any provider
        let any = serde_json::json!({
            "schema_version": 1,
            "forwarding": { "enabled": true, "secrets": {
                "t": { "header": "Authorization", "allow": { "providers": "any", "capabilities": ["*"] } }
            }}
        });
        assert!(matches!(
            load_json(any).await.unwrap_err(),
            StoreError::Corrupt { .. }
        ));

        // cookie secret without the recorded acknowledgement
        let cookie = serde_json::json!({
            "schema_version": 1,
            "forwarding": { "enabled": true, "secrets": {
                "s": { "header": "Cookie", "allow": { "providers": ["n"], "capabilities": ["*"] } }
            }}
        });
        assert!(matches!(
            load_json(cookie).await.unwrap_err(),
            StoreError::Corrupt { .. }
        ));

        // security-sensitive plain header
        let plain = serde_json::json!({
            "schema_version": 1,
            "forwarding": { "enabled": true, "plain_headers": {
                "Authorization": { "allow": { "providers": "any", "capabilities": ["*"] } }
            }}
        });
        assert!(matches!(
            load_json(plain).await.unwrap_err(),
            StoreError::Corrupt { .. }
        ));
    }

    #[tokio::test]
    async fn load_rejects_a_malformed_ref_name() {
        // The ref-name charset/length rule is enforced at write time; load must
        // re-enforce it, or a hand-edited config keyed by a raw token loads
        // active and leaks the token through the audit surface.
        for bad_ref in ["ghp_LIVETOKENvalueABC123", "Github-Token", "has space", ""] {
            // Build the secrets map explicitly so the ref name is a *dynamic*
            // key (a bare variable in `json!{{ key: .. }}` would be taken as a
            // literal).
            let mut secrets = serde_json::Map::new();
            secrets.insert(
                bad_ref.to_string(),
                serde_json::json!({
                    "header": "Authorization",
                    "allow": { "providers": ["node-1"], "capabilities": ["*"] }
                }),
            );
            let json = serde_json::json!({
                "schema_version": 1,
                "forwarding": { "enabled": true, "secrets": serde_json::Value::Object(secrets) }
            });
            assert!(
                matches!(
                    load_json(json).await.unwrap_err(),
                    StoreError::Corrupt { .. }
                ),
                "ref name {bad_ref:?} must be rejected on load",
            );
        }
    }

    #[tokio::test]
    async fn load_rejects_an_unknown_schema_version() {
        let future =
            serde_json::json!({ "schema_version": 999, "forwarding": { "enabled": false } });
        assert!(matches!(
            load_json(future).await.unwrap_err(),
            StoreError::Corrupt { .. }
        ));
    }

    #[tokio::test]
    async fn load_rejects_an_unknown_top_level_field() {
        // A typo'd top-level field must fail closed, not be silently ignored.
        let json = serde_json::json!({
            "schema_version": 1,
            "forwarding": { "enabled": false },
            "enabledd": true
        });
        assert!(matches!(
            load_json(json).await.unwrap_err(),
            StoreError::Corrupt { .. }
        ));
    }

    #[tokio::test]
    async fn load_accepts_a_valid_persisted_policy() {
        let ok = serde_json::json!({
            "schema_version": 1,
            "forwarding": { "enabled": true, "secrets": {
                "t": { "header": "Authorization", "allow": { "providers": ["n"], "capabilities": ["*"] } }
            }}
        });
        assert!(load_json(ok).await.is_ok());
    }

    #[tokio::test]
    async fn rejected_edit_in_mutate_saves_nothing() {
        let (_dir, path) = store_path();
        // First establish an enabled store with one grant.
        ForwardingStore::mutate(path.clone(), |s| {
            s.set_enabled(true);
            s.set_secret("keep", "Authorization", AllowList::default(), None, false)
        })
        .await
        .unwrap();
        // A transaction whose closure errors must not persist a partial change.
        let result = ForwardingStore::mutate(path.clone(), |s| {
            s.set_enabled(false); // mutated before the failing edit
            s.set_secret("bad", "Connection", AllowList::default(), None, false)
        })
        .await;
        assert!(result.is_err());
        let store = ForwardingStore::load(&path).await.unwrap();
        assert!(store.is_enabled(), "the failed transaction did not persist");
        assert!(store.config().secrets.contains_key("keep"));
        assert!(!store.config().secrets.contains_key("bad"));
    }

    #[tokio::test]
    async fn save_leaves_no_temp_file_behind() {
        let (_dir, path) = store_path();
        ForwardingStore::mutate(path.clone(), |s| {
            s.set_secret("t", "Authorization", AllowList::default(), None, false)
        })
        .await
        .unwrap();
        assert!(tokio::fs::metadata(&path).await.is_ok(), "store persisted");
        // No sibling `.tmp.<pid>` file lingers next to the store.
        let dir = path.parent().unwrap();
        let mut entries = tokio::fs::read_dir(dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            assert!(
                !name.to_string_lossy().contains(".tmp."),
                "a temp file was left behind: {name:?}",
            );
        }
    }

    #[tokio::test]
    async fn audit_is_value_free_and_renders() {
        let (_dir, path) = store_path();
        let mut store = ForwardingStore::load(&path).await.unwrap();
        store.set_enabled(true);
        store
            .set_secret(
                "github-token",
                "Authorization",
                allow(ProviderScope::Ids(vec!["node-1".into()]), &["github.*"]),
                Some("github-api".into()),
                false,
            )
            .unwrap();
        store
            .set_plain_header("X-Trace-Id", allow(ProviderScope::Any, &["*"]))
            .unwrap();

        let audit = store.audit();
        assert!(audit.enabled);
        assert_eq!(audit.secret_grants.len(), 1);
        assert_eq!(audit.plain_grants.len(), 1);
        let g = &audit.secret_grants[0];
        assert_eq!(g.ref_name, "github-token");
        assert_eq!(g.header, "authorization");
        assert_eq!(g.providers, "node-1");
        assert_eq!(g.capabilities, vec!["github.*".to_string()]);
        assert_eq!(g.purpose.as_deref(), Some("github-api"));

        // The rendering names refs, headers, scopes — but there is no value
        // anywhere in the store to leak in the first place.
        let text = audit.render();
        assert!(text.contains("ENABLED"));
        assert!(text.contains("github-token -> authorization"));
        assert!(text.contains("providers=node-1"));
        assert!(text.contains("x-trace-id"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn saved_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = store_path();
        ForwardingStore::mutate(path.clone(), |s| {
            s.set_secret("t", "Authorization", AllowList::default(), None, false)
        })
        .await
        .unwrap();
        let mode = tokio::fs::metadata(&path)
            .await
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "forwarding policy must be owner-only");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_mutations_do_not_lose_updates() {
        let (_dir, path) = store_path();
        let (r1, r2) = tokio::join!(
            ForwardingStore::mutate(path.clone(), |s| {
                s.set_secret("a", "Authorization", AllowList::default(), None, false)
            }),
            ForwardingStore::mutate(path.clone(), |s| {
                s.set_secret("b", "Authorization", AllowList::default(), None, false)
            }),
        );
        r1.unwrap();
        r2.unwrap();
        let store = ForwardingStore::load(&path).await.unwrap();
        assert!(store.config().secrets.contains_key("a"), "first survived");
        assert!(store.config().secrets.contains_key("b"), "second survived");
    }
}
