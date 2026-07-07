//! Bundled Mozilla WebPKI trust roots for the money-path HTTP clients.
//!
//! reqwest 0.13 defaults the rustls backend to `rustls-platform-verifier`
//! (the OS trust store) and dropped the bundled-roots cargo feature the
//! pre-0.13 `rustls-tls` build relied on. The facilitator, settlement, and
//! x402 clients instead pin the compiled-in Mozilla root set via
//! [`ClientBuilder::tls_certs_only`](reqwest::ClientBuilder::tls_certs_only),
//! so the money path's trust anchors are deterministic and hermetic:
//! identical across every deploy target (scratch/distroless containers,
//! CI, dev boxes), never dependent on an OS store that might be absent,
//! stale, or locally augmented with a corporate MITM root. This restores
//! the exact trust posture the crate had under reqwest 0.12.

/// The compiled-in Mozilla server-auth roots, as `reqwest::Certificate`s.
///
/// Pass the result to `tls_certs_only` on every money-path client builder:
///
/// ```ignore
/// let http = reqwest::Client::builder()
///     .tls_certs_only(crate::tls_roots::webpki_roots()?)
///     .build()?;
/// ```
///
/// For the rustls backend `Certificate::from_der` merely wraps the DER
/// (the certificates are parsed when the root store is assembled at
/// `build()`), so this is effectively infallible; the `Result` is
/// propagated rather than unwrapped to keep the money path panic-free.
pub(crate) fn webpki_roots() -> reqwest::Result<Vec<reqwest::Certificate>> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| reqwest::Certificate::from_der(der.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled set must be present and non-trivial. If a dependency
    /// bump ever ships an empty root list, `tls_certs_only` would build a
    /// client that trusts nothing and every money-path request would
    /// fail closed — loud here is better than a fleet-wide outage.
    #[test]
    fn webpki_roots_are_present_and_parse() {
        let roots = webpki_roots().expect("bundled webpki roots parse as DER");
        // Mozilla's server-auth set is ~150 roots; guard well under that.
        assert!(
            roots.len() > 100,
            "expected the full Mozilla root set, got {}",
            roots.len()
        );
    }

    /// End-to-end proof that the pinned roots verify a real-world chain:
    /// a client built exactly like the money-path clients
    /// (`tls_certs_only(webpki_roots())`, no OS store) completes a live
    /// TLS handshake against a public CA-signed host. Ignored by default —
    /// requires network egress, like the other live-endpoint tests.
    #[tokio::test]
    #[ignore = "requires network egress"]
    async fn pinned_roots_verify_a_real_https_chain() {
        let http = reqwest::Client::builder()
            .tls_certs_only(webpki_roots().expect("roots"))
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
