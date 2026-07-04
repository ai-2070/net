//! Secret **value** backend + resolver
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 1, value side).
//!
//! The policy store ([`super::store`]) records *that* a secret ref may go to a
//! provider; this is the other half — *where the value lives* and how it is
//! turned into something injectable. The two are deliberately split: policy is
//! auditable and value-free; values live behind [`SecretBackend`], reachable
//! only as a [`ForwardedHeaderValue`] and only after policy has already said
//! yes.
//!
//! [`SecretBackend`] is the seam. The concrete at-rest backends — OS keychain
//! (macOS Keychain / Windows Credential Manager / Linux Secret Service) and an
//! encrypted file — plug in here without touching the policy store, the object,
//! or the resolver. This module ships the trait and an
//! [`InMemorySecretBackend`] (ephemeral / test); a persistent backend is a
//! drop-in, and choosing one is a security-posture decision made explicitly,
//! not defaulted to plaintext-on-disk.
//!
//! **Values enter through the operator, never the model.** A backend's `set`
//! path is a CLI / keychain operation; nothing here reads a value out of a tool
//! argument, an A2A message, or agent-generated config. And nothing here
//! forwards: [`resolve_secret_send`] *materializes* a value locally for the
//! injection boundary; sealing and transmission are Phase 2.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::fmt;

use super::header::{zeroize_vec, ForwardedHeaderValue, HeaderError, HeaderName};
use super::policy::{DenialLevel, ForwardingConfig};

/// A zeroize-on-drop container for a stored secret value. Keeping the map's
/// values in this wrapper — rather than a bare `Vec<u8>` — means a removed,
/// overwritten, or dropped value has its backing allocation scrubbed instead of
/// lingering in freed process memory.
#[derive(Default)]
struct StoredSecret(Vec<u8>);

impl Drop for StoredSecret {
    fn drop(&mut self) {
        zeroize_vec(&mut self.0);
    }
}

/// A failure reading from (or writing to) a secret value backend.
#[derive(Debug, thiserror::Error)]
pub enum SecretBackendError {
    /// The backend itself failed (keychain unavailable, I/O error, …). The
    /// message never contains a secret value.
    #[error("secret backend error: {0}")]
    Backend(String),
    /// A stored value could not be wrapped (control characters, oversize) — a
    /// corrupt or mis-entered value, surfaced without printing it.
    #[error(transparent)]
    Value(#[from] HeaderError),
}

/// A store of secret **values**, keyed by ref name. Implementations back onto
/// the OS keychain, an encrypted file, or (here) memory. The trait yields a
/// value only as a [`ForwardedHeaderValue`], so a backend can't leak a raw
/// `String`/`Vec<u8>` into a log or error through this surface.
#[async_trait]
pub trait SecretBackend: Send + Sync {
    /// Fetch the value for `ref_name`, or `None` if no value is stored. The
    /// policy store may know the ref while the value has not been entered yet —
    /// that distinction is the caller's to handle (see [`resolve_secret_send`]).
    async fn get(&self, ref_name: &str)
        -> Result<Option<ForwardedHeaderValue>, SecretBackendError>;

    /// Whether a value exists for `ref_name`, without materializing it.
    async fn contains(&self, ref_name: &str) -> Result<bool, SecretBackendError>;
}

/// An in-memory, non-persistent secret backend for tests and ephemeral use.
/// Values live only for the process lifetime and are scrubbed on drop via the
/// wrapper type. **Not** a production store — it keeps plaintext in process
/// memory and nowhere else, which is exactly why it's the safe default until a
/// real at-rest backend is chosen.
///
/// It is immutable-after-construction: build it up with [`Self::with`] /
/// [`Self::set`] at operator/CLI time, then share it (`&dyn SecretBackend`) for
/// lookups. That keeps the read path lock-free — a real backend backs onto the
/// OS keychain / an encrypted file, where the OS handles concurrency.
#[derive(Default)]
pub struct InMemorySecretBackend {
    values: BTreeMap<String, StoredSecret>,
}

impl InMemorySecretBackend {
    /// A fresh, empty backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style value entry (consumes and returns `self`) for setup.
    #[must_use]
    pub fn with(mut self, ref_name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.set(ref_name, value);
        self
    }

    /// Enter or replace a value. This is the operator/CLI entry path — never
    /// call it with a model-supplied value.
    pub fn set(&mut self, ref_name: impl Into<String>, value: impl Into<Vec<u8>>) {
        // Any prior value at this key is dropped here → scrubbed by StoredSecret.
        self.values.insert(ref_name.into(), StoredSecret(value.into()));
    }

    /// Remove a value. Returns whether one was present.
    pub fn remove(&mut self, ref_name: &str) -> bool {
        self.values.remove(ref_name).is_some()
    }

    /// The number of stored refs (not their values).
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the backend holds no values.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Redacted — prints only the ref count, never a value or even a ref name.
impl fmt::Debug for InMemorySecretBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InMemorySecretBackend({} refs)", self.values.len())
    }
}

#[async_trait]
impl SecretBackend for InMemorySecretBackend {
    async fn get(
        &self,
        ref_name: &str,
    ) -> Result<Option<ForwardedHeaderValue>, SecretBackendError> {
        match self.values.get(ref_name) {
            Some(s) => Ok(Some(ForwardedHeaderValue::new(s.0.clone())?)),
            None => Ok(None),
        }
    }

    async fn contains(&self, ref_name: &str) -> Result<bool, SecretBackendError> {
        Ok(self.values.contains_key(ref_name))
    }
}

/// Why resolving a secret send failed — the three distinct outcomes a caller
/// must tell apart: policy said no (and at which level), the ref is allowed but
/// has no stored value yet, or the backend errored.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The caller policy denied the send at the named level. Carries no value.
    #[error("forwarding denied at the {0} level")]
    Denied(DenialLevel),
    /// Policy allowed the send, but no value is stored for the ref — the
    /// operator hasn't entered it. Distinct from a denial so the caller can
    /// prompt for entry rather than report a permission failure.
    #[error("no value is stored for secret ref {ref_name:?}")]
    ValueMissing {
        /// The ref with no value.
        ref_name: String,
    },
    /// The backend errored.
    #[error(transparent)]
    Backend(#[from] SecretBackendError),
}

impl From<DenialLevel> for ResolveError {
    fn from(level: DenialLevel) -> Self {
        ResolveError::Denied(level)
    }
}

/// Resolve a secret send: apply the caller policy, and only if it permits,
/// fetch the value. **Policy is checked before the backend is touched** — a
/// denied send never causes a value lookup (the same "unauthorized callers
/// never trigger a decrypt" discipline the plan's destination order uses).
/// Returns the wire header and the value to inject; forwards nothing.
pub async fn resolve_secret_send(
    config: &ForwardingConfig,
    backend: &dyn SecretBackend,
    secret_ref: &str,
    provider: &str,
    capability: &str,
) -> Result<(HeaderName, ForwardedHeaderValue), ResolveError> {
    let grant = config.decide_secret(secret_ref, provider, capability)?;
    let value = backend
        .get(secret_ref)
        .await?
        .ok_or_else(|| ResolveError::ValueMissing {
            ref_name: secret_ref.to_string(),
        })?;
    Ok((grant.header, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> ForwardingConfig {
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "secrets": {
                "github-token": {
                    "header": "Authorization",
                    "allow": { "providers": ["node-1"], "capabilities": ["github.*"] }
                }
            }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn in_memory_backend_get_contains_remove() {
        let mut b = InMemorySecretBackend::new();
        assert!(b.is_empty());
        b.set("github-token", b"Bearer ghp_secret".to_vec());
        assert!(b.contains("github-token").await.unwrap());
        assert!(!b.contains("missing").await.unwrap());
        let v = b.get("github-token").await.unwrap().unwrap();
        assert_eq!(v.expose(), b"Bearer ghp_secret");
        assert!(b.get("missing").await.unwrap().is_none());
        assert!(b.remove("github-token"));
        assert!(!b.contains("github-token").await.unwrap());
    }

    #[test]
    fn in_memory_backend_debug_is_redacted() {
        let b = InMemorySecretBackend::new().with("github-token", b"ghp_SECRET".to_vec());
        let dbg = format!("{b:?}");
        assert!(!dbg.contains("SECRET"), "backend Debug leaked a value: {dbg}");
        assert!(!dbg.contains("github-token"), "backend Debug leaked a ref name");
        assert!(dbg.contains("1 refs"));
    }

    #[tokio::test]
    async fn resolve_success_returns_header_and_value() {
        let config = enabled_config();
        let backend = InMemorySecretBackend::new().with("github-token", b"Bearer ghp_secret".to_vec());
        let (header, value) =
            resolve_secret_send(&config, &backend, "github-token", "node-1", "github.issues")
                .await
                .unwrap();
        assert_eq!(header.as_str(), "authorization");
        assert_eq!(value.expose(), b"Bearer ghp_secret");
    }

    #[tokio::test]
    async fn resolve_denied_never_touches_the_backend() {
        let config = enabled_config();
        // A backend that panics if read — proves policy is checked first.
        struct Exploding;
        #[async_trait]
        impl SecretBackend for Exploding {
            async fn get(
                &self,
                _: &str,
            ) -> Result<Option<ForwardedHeaderValue>, SecretBackendError> {
                panic!("backend must not be consulted after a policy denial");
            }
            async fn contains(&self, _: &str) -> Result<bool, SecretBackendError> {
                panic!("backend must not be consulted after a policy denial");
            }
        }
        // Wrong provider → per-identity denial, before any value lookup.
        let err = resolve_secret_send(&config, &Exploding, "github-token", "node-evil", "github.x")
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::Denied(DenialLevel::PerIdentity)));
    }

    #[tokio::test]
    async fn resolve_allowed_but_no_value_is_distinct_from_denied() {
        let config = enabled_config();
        let backend = InMemorySecretBackend::new(); // empty — value not entered
        let err = resolve_secret_send(&config, &backend, "github-token", "node-1", "github.x")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ResolveError::ValueMissing { ref_name } if ref_name == "github-token"),
            "a missing value must not look like a permission denial",
        );
    }

    #[tokio::test]
    async fn resolve_off_switch_denies_globally() {
        let mut config = enabled_config();
        config.enabled = false;
        let backend = InMemorySecretBackend::new().with("github-token", b"x".to_vec());
        let err = resolve_secret_send(&config, &backend, "github-token", "node-1", "github.x")
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::Denied(DenialLevel::Global)));
    }
}
