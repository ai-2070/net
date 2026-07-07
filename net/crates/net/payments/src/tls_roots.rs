//! Hermetic TLS for the money-path HTTP clients: an explicit rustls
//! [`ClientConfig`](rustls::ClientConfig) pinned to the ring crypto
//! provider and the bundled Mozilla trust roots, handed to reqwest via
//! [`use_preconfigured_tls`](reqwest::ClientBuilder::use_preconfigured_tls).
//!
//! Two deliberate departures from reqwest 0.13's defaults, both for the
//! money path:
//!
//! * **Provider — ring, no process global.** reqwest is built with
//!   `rustls-no-provider` so the build pulls ring, not aws-lc-rs (no
//!   aws-lc-sys / cmake / C toolchain). We supply the provider *explicitly*
//!   to [`ClientConfig::builder_with_provider`] rather than installing a
//!   process-global default. `CryptoProvider::install_default` would leak
//!   into every other rustls user in the host process — notably whatever
//!   embeds the Python extension — and race whoever installs first.
//!   reqwest consumes a preconfigured `ClientConfig` verbatim (its
//!   `TlsBackend::BuiltRustls` path), so its own provider resolution — the
//!   part that panics under `rustls-no-provider` — is never reached and no
//!   global is ever touched.
//!
//! * **Roots — bundled, not the OS store.** reqwest 0.13 defaults to the
//!   platform verifier and dropped its bundled-roots feature. We pin the
//!   compiled-in Mozilla set (`webpki-root-certs`) so the money path's
//!   trust anchors are deterministic and hermetic: identical across every
//!   deploy target (scratch/distroless containers, CI, dev boxes), never an
//!   OS store that might be absent, stale, or augmented with a corporate
//!   MITM root. This matches the pre-0.13 `rustls-tls` posture.

use std::sync::Arc;

/// Build the money-path rustls [`ClientConfig`](rustls::ClientConfig): the
/// ring provider, TLS 1.2/1.3, and **only** the bundled Mozilla roots (the
/// OS store is ignored). Pass the result to
/// [`use_preconfigured_tls`](reqwest::ClientBuilder::use_preconfigured_tls)
/// on every money-path client builder.
///
/// The `rustls` version here must stay in lockstep with reqwest's (both
/// pinned to 0.23 in `Cargo.toml`): reqwest downcasts the preconfigured
/// value by concrete type, so a version skew degrades to a loud "unknown
/// TLS backend" error at `build()` — exercised by
/// `client_builds_with_preconfigured_ring`.
pub(crate) fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    // Every entry is a compiled-in Mozilla CA cert, so `added` equals the
    // set size and `ignored` is 0; an empty store would make the client
    // trust nothing and fail closed. The count is guarded by
    // `webpki_roots_are_present` rather than asserted on the hot path.
    let (_added, _ignored) =
        roots.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());

    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();

    // reqwest's from-scratch path sets ALPN from its version preference; its
    // preconfigured path leaves our config untouched, so set it here. No
    // `http2` feature is enabled, so HTTP/1.1 is the only protocol.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled root set must be present and non-trivial. If a dependency
    /// bump ever ships an empty list, the client would trust nothing and
    /// every money-path request would fail closed — loud here beats a
    /// fleet-wide outage.
    #[test]
    fn webpki_roots_are_present() {
        // Mozilla's server-auth set is ~150 roots; guard well under that.
        assert!(
            webpki_root_certs::TLS_SERVER_ROOT_CERTS.len() > 100,
            "expected the full Mozilla root set, got {}",
            webpki_root_certs::TLS_SERVER_ROOT_CERTS.len()
        );
    }

    /// The config assembles with the ring provider and the pinned roots.
    #[test]
    fn tls_config_builds_with_ring() {
        let config = tls_config().expect("rustls config builds with ring + pinned roots");
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    /// Offline proof the whole path wires up with **no** process-global
    /// provider installed: reqwest accepts the preconfigured ring config
    /// (the concrete-type downcast succeeds — a rustls/reqwest version skew
    /// would error here) and builds a client.
    #[test]
    fn client_builds_with_preconfigured_ring() {
        reqwest::Client::builder()
            .use_preconfigured_tls(tls_config().expect("config"))
            .build()
            .expect("client builds from the preconfigured ring config");
    }

    /// End-to-end proof the pinned roots verify a real-world chain: a client
    /// trusting only the bundled roots (no OS store, ring provider)
    /// completes a live TLS handshake against a public CA-signed host.
    /// Ignored by default — requires network egress.
    #[tokio::test]
    #[ignore = "requires network egress"]
    async fn pinned_roots_verify_a_real_https_chain() {
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls_config().expect("config"))
            .build()
            .expect("client builds");
        // Any response proves the handshake verified against the bundled
        // roots; a TLS failure would surface as an `Err` here.
        let resp = http
            .get("https://cloudflare.com")
            .send()
            .await
            .expect("live TLS handshake against a public chain succeeds");
        assert!(resp.status().as_u16() > 0);
    }
}
