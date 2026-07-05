//! OS-keychain secret value backend
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 1, value side).
//!
//! A [`SecretBackend`](super::SecretBackend) backed by the platform credential
//! store — macOS Keychain, Windows Credential Manager, or the Linux kernel
//! `keyutils` — via `keyring-core` and the platform's native store crate. The
//! OS handles storage and per-user access control, so nothing is ever written to
//! a plaintext file by Net.
//!
//! We link `keyring-core` + each native store crate directly rather than the
//! `keyring` façade, and set the process default store ourselves
//! ([`ensure_default_store`]): keyring 4's `v1` compat feature force-selects the
//! Secret Service (zbus) store on Linux, which needs a running D-Bus session a
//! headless host lacks. Picking `keyutils` directly keeps the Linux backend pure
//! Rust, dbus-free, and headless-safe. The trade-off is persistence: a keyutils
//! keyring lives for the login session, not across reboots — see the
//! `[dependencies]` note in `Cargo.toml` for how to swap in Secret Service where
//! reboot-persistence matters.
//!
//! Gated behind the non-default **`keychain`** cargo feature: it pulls the
//! keyring-core stack and needs a keychain-capable host to run, so the default
//! build / CI / `cargo install net-mesh-mcp` never compile it.
//!
//! Values enter through the operator ([`Self::set`], wired to `net secret set`),
//! never the model, and every keychain call runs on a blocking thread so a
//! syscall can't stall the async runtime. A value surfaces only as a
//! [`ForwardedHeaderValue`]; keyring-core errors describe the *operation*, never
//! the value, so mapping them to a string can't leak a secret.

use async_trait::async_trait;

use super::header::{zeroize_vec, ForwardedHeaderValue};
use super::secret::{SecretBackend, SecretBackendError};

/// The default keychain **service** namespace forwarded secrets live under.
/// A ref name is the keychain "account" within this service.
pub const DEFAULT_KEYCHAIN_SERVICE: &str = "net-mesh-forwarding";

/// A [`SecretBackend`] backed by the OS keychain. Cheap to clone/construct — it
/// holds only the service namespace; every operation opens a fresh
/// `keyring_core::Entry`.
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
        // Validate at entry with the same rules `get` enforces (via
        // `ForwardedHeaderValue::new`), so an oversize / control-char value that
        // could never be read back is rejected here rather than persisted and
        // then failing at forward time. Runs before the keychain is touched.
        ForwardedHeaderValue::validate(value)?;
        let (service, ref_name, mut value) =
            (self.service.clone(), ref_name.to_string(), value.to_vec());
        run_blocking(move || {
            let result = entry(&service, &ref_name)?
                .set_secret(&value)
                .map_err(map_err);
            // Scrub the plaintext copy we materialized for the keychain call,
            // regardless of outcome — every other path in this module
            // (`contains`, `get`, `ForwardedHeaderValue`) scrubs its buffer, and
            // `set` was the one that dropped its copy in the clear.
            zeroize_vec(&mut value);
            result
        })
        .await
    }

    /// Delete the value for `ref_name`. Returns whether one existed.
    pub async fn delete(&self, ref_name: &str) -> Result<bool, SecretBackendError> {
        let (service, ref_name) = (self.service.clone(), ref_name.to_string());
        run_blocking(
            move || match entry(&service, &ref_name)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring_core::Error::NoEntry) => Ok(false),
                Err(e) => Err(map_err(e)),
            },
        )
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
            Err(keyring_core::Error::NoEntry) => Ok(None),
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
            // keyring-core has no existence probe that skips the value, so we
            // read it — but we only want existence, so scrub the plaintext we had
            // to materialize before it drops (matching the module's
            // secret-handling guarantees).
            Ok(mut bytes) => {
                zeroize_vec(&mut bytes);
                Ok(true)
            }
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_err(e)),
        })
        .await
    }
}

/// Open a keychain entry for `(service, ref_name)`, ensuring the process's
/// default credential store is our chosen native store first.
fn entry(service: &str, ref_name: &str) -> Result<keyring_core::Entry, SecretBackendError> {
    ensure_default_store()?;
    keyring_core::Entry::new(service, ref_name).map_err(map_err)
}

/// Map a `keyring-core` error onto the backend error. The message describes the
/// operation / platform, never the value.
fn map_err(e: keyring_core::Error) -> SecretBackendError {
    SecretBackendError::Backend(e.to_string())
}

/// Install the platform's native credential store as `keyring-core`'s process
/// default, exactly once. `keyring-core` resolves every [`keyring_core::Entry`]
/// through a global default store, so we must set it before the first `Entry` —
/// and we set it *ourselves*, per-OS, rather than via the `keyring` façade,
/// specifically so Linux uses `keyutils` (headless-safe, no dbus) and never the
/// Secret Service store keyring's `v1` compat would otherwise force.
fn ensure_default_store() -> Result<(), SecretBackendError> {
    use std::sync::OnceLock;
    // The one-time init result, cached so a store-construction failure surfaces
    // on every call (not just the first). `String` because `SecretBackendError`
    // is not `Clone`.
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    match INIT.get_or_init(|| set_platform_store().map_err(|e| e.to_string())) {
        Ok(()) => Ok(()),
        Err(msg) => Err(SecretBackendError::Backend(msg.clone())),
    }
}

/// Construct the host platform's native store and register it as the default.
/// Each store crate is target-gated in `Cargo.toml`, so only the matching arm
/// compiles; an unsupported platform sets no store and every `Entry::new` then
/// fails with a `keyring-core` "no default store" error (surfaced, not panicked).
#[allow(clippy::unnecessary_wraps)]
fn set_platform_store() -> Result<(), keyring_core::Error> {
    #[cfg(target_os = "linux")]
    keyring_core::set_default_store(linux_keyutils_keyring_store::Store::new()?);
    #[cfg(target_os = "windows")]
    keyring_core::set_default_store(windows_native_keyring_store::Store::new()?);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    keyring_core::set_default_store(apple_native_keyring_store::keychain::Store::new()?);
    Ok(())
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
    async fn set_rejects_invalid_value_before_touching_the_keychain() {
        // Validation runs before any keychain syscall, so this holds even on a
        // host with no usable keychain: an oversize or control-char value that
        // `get` could never read back is refused at entry, not persisted.
        let backend = KeychainSecretBackend::new("svc-unused");
        let oversize = vec![b'x'; crate::forward::MAX_HEADER_VALUE_LEN + 1];
        assert!(matches!(
            backend.set("kc-test", &oversize).await.unwrap_err(),
            SecretBackendError::Value(_),
        ));
        assert!(matches!(
            backend.set("kc-test", b"tok\nmore").await.unwrap_err(),
            SecretBackendError::Value(_),
        ));
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
