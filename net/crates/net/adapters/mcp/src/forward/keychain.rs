//! OS-keychain secret value backend
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 1, value side).
//!
//! A persistent [`SecretBackend`](super::SecretBackend) backed by the platform
//! credential store — macOS Keychain, Windows Credential Manager, or the Linux
//! Secret Service — via the `keyring` crate. This is the recommended at-rest
//! home for forwarded secret values: the OS handles encryption, per-user
//! scoping, and access control, so nothing is ever written to a plaintext file
//! by Net.
//!
//! Gated behind the non-default **`keychain`** cargo feature: it pulls the
//! `keyring` dependency and needs a keychain-capable host to run, so the
//! default build / CI / `cargo install net-mesh-mcp` never compile it.
//!
//! Values enter through the operator ([`Self::set`], wired to `net secret set`),
//! never the model, and every keychain call runs on a blocking thread so a
//! syscall can't stall the async runtime. A value surfaces only as a
//! [`ForwardedHeaderValue`]; `keyring` errors describe the *operation*, never
//! the value, so mapping them to a string can't leak a secret.

use async_trait::async_trait;

use super::header::{zeroize_vec, ForwardedHeaderValue};
use super::secret::{SecretBackend, SecretBackendError};

/// The default keychain **service** namespace forwarded secrets live under.
/// A ref name is the keychain "account" within this service.
pub const DEFAULT_KEYCHAIN_SERVICE: &str = "net-mesh-forwarding";

/// A [`SecretBackend`] backed by the OS keychain. Cheap to clone/construct — it
/// holds only the service namespace; every operation opens a fresh
/// `keyring::Entry`.
#[derive(Debug, Clone)]
pub struct KeychainSecretBackend {
    service: String,
}

impl Default for KeychainSecretBackend {
    fn default() -> Self {
        Self::new(DEFAULT_KEYCHAIN_SERVICE)
    }
}

impl KeychainSecretBackend {
    /// A backend storing under keychain `service`. Use [`Self::default`] for the
    /// standard [`DEFAULT_KEYCHAIN_SERVICE`] namespace.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Enter or replace a secret value for `ref_name`. The operator/CLI entry
    /// path (`net secret set`) — never call it with a model-supplied value.
    pub async fn set(&self, ref_name: &str, value: &[u8]) -> Result<(), SecretBackendError> {
        let (service, ref_name, value) =
            (self.service.clone(), ref_name.to_string(), value.to_vec());
        run_blocking(move || {
            entry(&service, &ref_name)?
                .set_secret(&value)
                .map_err(map_err)
        })
        .await
    }

    /// Delete the value for `ref_name`. Returns whether one existed.
    pub async fn delete(&self, ref_name: &str) -> Result<bool, SecretBackendError> {
        let (service, ref_name) = (self.service.clone(), ref_name.to_string());
        run_blocking(move || match entry(&service, &ref_name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_err(e)),
        })
        .await
    }
}

#[async_trait]
impl SecretBackend for KeychainSecretBackend {
    async fn get(
        &self,
        ref_name: &str,
    ) -> Result<Option<ForwardedHeaderValue>, SecretBackendError> {
        let (service, ref_name) = (self.service.clone(), ref_name.to_string());
        let bytes = run_blocking(move || match entry(&service, &ref_name)?.get_secret() {
            Ok(b) => Ok(Some(b)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        })
        .await?;
        match bytes {
            Some(b) => Ok(Some(ForwardedHeaderValue::new(b)?)),
            None => Ok(None),
        }
    }

    async fn contains(&self, ref_name: &str) -> Result<bool, SecretBackendError> {
        let (service, ref_name) = (self.service.clone(), ref_name.to_string());
        run_blocking(move || match entry(&service, &ref_name)?.get_secret() {
            // keyring has no existence probe that skips the value, so we read
            // it — but we only want existence, so scrub the plaintext we had to
            // materialize before it drops (matching the module's secret-handling
            // guarantees).
            Ok(mut bytes) => {
                zeroize_vec(&mut bytes);
                Ok(true)
            }
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_err(e)),
        })
        .await
    }
}

/// Open a keychain entry for `(service, ref_name)`.
fn entry(service: &str, ref_name: &str) -> Result<keyring::Entry, SecretBackendError> {
    keyring::Entry::new(service, ref_name).map_err(map_err)
}

/// Map a `keyring` error onto the backend error. The message describes the
/// operation / platform, never the value.
fn map_err(e: keyring::Error) -> SecretBackendError {
    SecretBackendError::Backend(e.to_string())
}

/// Run a blocking keychain closure on the blocking pool so the syscall can't
/// stall the async runtime.
async fn run_blocking<T, F>(f: F) -> Result<T, SecretBackendError>
where
    F: FnOnce() -> Result<T, SecretBackendError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SecretBackendError::Backend(format!("keychain task panicked: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against the *real* platform keychain. On a host with no
    /// usable credential store (e.g. headless CI), the first operation errors
    /// and the test skips rather than failing — it only runs under
    /// `--features keychain` in the first place.
    #[tokio::test]
    async fn keychain_round_trip() {
        // A per-process service so parallel runs / leftovers don't collide.
        let service = format!("net-mesh-forwarding-test-{}", std::process::id());
        let backend = KeychainSecretBackend::new(service);
        let ref_name = "kc-test-ref";

        if let Err(e) = backend.set(ref_name, b"Bearer kc-secret").await {
            eprintln!("skipping keychain round-trip — no usable keychain: {e}");
            return;
        }

        assert!(backend.contains(ref_name).await.unwrap());
        let value = backend.get(ref_name).await.unwrap().unwrap();
        assert_eq!(value.expose(), b"Bearer kc-secret");

        assert!(backend.delete(ref_name).await.unwrap());
        assert!(
            backend.get(ref_name).await.unwrap().is_none(),
            "value is gone after delete"
        );
        assert!(
            !backend.delete(ref_name).await.unwrap(),
            "deleting an absent value is a no-op"
        );
    }

    #[tokio::test]
    async fn missing_ref_is_none_not_error() {
        let service = format!("net-mesh-forwarding-test-absent-{}", std::process::id());
        let backend = KeychainSecretBackend::new(service);
        // A never-set ref must read back as None (or skip if no keychain).
        match backend.get("definitely-absent").await {
            Ok(v) => assert!(v.is_none(), "absent ref must be None"),
            Err(e) => eprintln!("skipping — no usable keychain: {e}"),
        }
    }

    #[test]
    fn debug_shows_service_not_a_value() {
        let backend = KeychainSecretBackend::new("svc");
        // The backend never holds a value, so Debug is inherently safe; assert
        // it identifies the service for operator legibility.
        assert!(format!("{backend:?}").contains("svc"));
    }
}
