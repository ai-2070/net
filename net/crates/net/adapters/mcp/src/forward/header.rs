//! Header names and secret header values for forwarded invocation context
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 0).
//!
//! Two load-bearing types live here:
//!
//! - [`HeaderName`] — a **canonicalized** (lowercased, token-validated) header
//!   name. Normalizing before any policy check is what defeats case games and
//!   duplicate-header smuggling (`Authorization` vs `authorization` vs
//!   `AUTHORIZATION` are one name), and the classification helpers mark the
//!   security-sensitive and hop-by-hop names the rest of the module gates on.
//! - [`ForwardedHeaderValue`] — a **secret wrapper** around a header value.
//!   Its whole job is to make the value hard to leak: no `Debug`/`Display`
//!   ever prints it, it has no `Serialize`, and the only way to read it is the
//!   explicit [`ForwardedHeaderValue::expose`] method, which the caller must
//!   name at the injection boundary. Generic error/log serialization can't
//!   capture what it can't reach (plan doctrine #5).
//!
//! Nothing here forwards anything. Phase 0 is spec-only: these are the shapes
//! and invariants future phases seal, ship, and inject — defined now so the
//! bridge can't smuggle in "just forward `Authorization`" without going
//! through them.

use std::fmt;

/// Maximum number of headers a single forwarded context may carry. Oversize
/// fails closed (`ContextError::TooManyHeaders`).
pub const MAX_FORWARDED_HEADERS: usize = 16;

/// Maximum byte length of a single forwarded header value. A bearer token or
/// tenant id is small; anything larger is refused rather than truncated.
pub const MAX_HEADER_VALUE_LEN: usize = 8 * 1024;

/// Maximum byte length of a header **name**. Real header names are short (the
/// longest standard ones are well under 40 bytes); this caps the name so it
/// counts against the forwarded-byte budget with a bound and so the canonical
/// AAD's length-prefixed name fields can never approach the `u32` prefix limit.
pub const MAX_HEADER_NAME_LEN: usize = 256;

/// Maximum combined byte length of all forwarded header values in one context.
pub const MAX_TOTAL_FORWARDED_BYTES: usize = 32 * 1024;

/// Header names that are ambient credentials or credential-adjacent. They are
/// single-value only (never folded), and accepting one auto-tags a capability
/// [`accepts_forwarded_credentials`](crate::forward::RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS).
/// Stored lowercased — compare against [`HeaderName::as_str`]. Widening this
/// beyond `authorization`/`cookie` is what stops a bearer credential riding the
/// **plain** path (or evading the risk tag) just because it uses a vendor
/// header like `x-api-key` instead of `Authorization`.
const SECURITY_SENSITIVE_EXACT: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "authentication",
    "x-auth-key",
    "x-functions-key",
];

/// Substrings that mark a header name credential-bearing even when it is not in
/// [`SECURITY_SENSITIVE_EXACT`] — catches vendor variants (`x-acme-api-key`,
/// `x-session-token`, `…-secret`) so a novel name can't slip a credential onto
/// the plain path. Deliberately narrow: bare `-key` / `-token` are **not**
/// listed, so benign idempotency / partition / trace / csrf headers stay
/// plain-eligible while real credential shapes (`api-key`, `auth-token`,
/// `access-token`, `session-token`, `security-token`, `secret`, `password`) do
/// not.
const SECURITY_SENSITIVE_SUBSTR: &[&str] = &[
    "apikey",
    "api-key",
    "api_key",
    "auth-token",
    "auth_token",
    "access-token",
    "access_token",
    "session-token",
    "session_token",
    "security-token",
    "secret",
    "password",
    "passwd",
];

/// Hop-by-hop headers (RFC 7230 §6.1) plus the `proxy-` family. These describe
/// a single transport hop, never end-to-end authority, so they are **never**
/// forwardable — blocked regardless of any allowlist. `proxy-*` is matched by
/// prefix so `proxy-authorization` (a per-hop credential) can't slip through.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Why building a [`HeaderName`] or [`ForwardedHeaderValue`] failed. Header
/// names are user-visible and appear in audit, so naming the offending name is
/// fine; a value's *content* never appears in any error (only its length).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    /// A header name was empty after trimming.
    #[error("empty header name")]
    EmptyName,
    /// A header name contained a byte outside the RFC 7230 `token` set.
    #[error("header name {name:?} contains an invalid character")]
    InvalidName {
        /// The rejected name (safe to surface — names are not secret).
        name: String,
    },
    /// A header name exceeded [`MAX_HEADER_NAME_LEN`].
    #[error("header name is {len} bytes, over the {max}-byte limit")]
    NameTooLong {
        /// The rejected name's length (names are not secret, but the length is
        /// all that's needed).
        len: usize,
        /// The configured limit.
        max: usize,
    },
    /// A header value carried a control byte (CR, LF, NUL, …). Rejecting these
    /// is the header-injection / response-splitting defense. The value itself
    /// is never included — only that it was rejected.
    #[error("header value contains a control character")]
    ControlCharInValue,
    /// A header value exceeded [`MAX_HEADER_VALUE_LEN`].
    #[error("header value is {len} bytes, over the {max}-byte limit")]
    ValueTooLong {
        /// The rejected value's length (not its content).
        len: usize,
        /// The configured limit.
        max: usize,
    },
}

/// A canonicalized HTTP header name: lowercased and validated to the RFC 7230
/// `token` grammar. Two names that differ only in case are equal here, which is
/// exactly what stops duplicate-header smuggling in a forwarded context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeaderName(String);

impl HeaderName {
    /// Parse and canonicalize `raw`. Surrounding ASCII whitespace is trimmed,
    /// the remainder is lowercased, and every byte must be an RFC 7230 `token`
    /// character (`ALPHA / DIGIT / !#$%&'*+-.^_`` `|~`` `). Empty or malformed
    /// names fail closed — a name we can't canonicalize is a name we won't
    /// forward.
    pub fn parse(raw: &str) -> Result<Self, HeaderError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(HeaderError::EmptyName);
        }
        if trimmed.len() > MAX_HEADER_NAME_LEN {
            return Err(HeaderError::NameTooLong {
                len: trimmed.len(),
                max: MAX_HEADER_NAME_LEN,
            });
        }
        if !trimmed.bytes().all(is_token_byte) {
            return Err(HeaderError::InvalidName {
                name: trimmed.to_string(),
            });
        }
        Ok(HeaderName(trimmed.to_ascii_lowercase()))
    }

    /// The canonical (lowercased) name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A credential or credential-adjacent header — `authorization`, `cookie`,
    /// `set-cookie`, and the vendor bearer-credential family (`x-api-key`,
    /// `x-auth-token`, `x-amz-security-token`, `…-secret`, …). Single-value only,
    /// and its acceptance flags a capability as taking forwarded credentials.
    /// Such a header can never ride the plain path — it must use the (stricter)
    /// secret path.
    pub fn is_security_sensitive(&self) -> bool {
        SECURITY_SENSITIVE_EXACT.contains(&self.0.as_str())
            || SECURITY_SENSITIVE_SUBSTR
                .iter()
                .any(|needle| self.0.contains(needle))
    }

    /// A hop-by-hop header (never end-to-end, so never forwardable). `proxy-*`
    /// is matched by prefix.
    pub fn is_hop_by_hop(&self) -> bool {
        HOP_BY_HOP.contains(&self.0.as_str()) || self.0.starts_with("proxy-")
    }

    /// May this header ever be forwarded end-to-end? Hop-by-hop headers never
    /// can. (Security-sensitive but end-to-end headers like `authorization`
    /// *can* be, subject to policy — that gate lives in
    /// [`ForwardingConfig`](crate::forward::ForwardingConfig).)
    pub fn is_forwardable(&self) -> bool {
        !self.is_hop_by_hop()
    }
}

impl fmt::Display for HeaderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// True if `b` is an RFC 7230 `token` character.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, |b'!'| b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~')
}

/// A forwarded header **value**, wrapped so it is hard to leak.
///
/// The security contract (plan doctrine #2, #5):
///
/// - **No `Debug`/`Display` output of the value.** Both are implemented to
///   print only a redaction marker and the length, so a value can never reach
///   a log line, a `{:?}` in an error, or a panic message by accident.
/// - **No `Serialize`.** The type is deliberately not serializable, so it
///   cannot ride a generic `serde` error/audit object. The sealed payload is
///   assembled explicitly at the seal boundary in a later phase.
/// - **Explicit exposure only.** [`Self::expose`] is the single read path; a
///   reader has to name it, which makes the injection boundary auditable.
/// - **Best-effort zeroize on drop.** The backing buffer is overwritten when
///   the value is dropped. This is best-effort — true zeroization (defeating
///   reallocation) arrives with the sealing work in Phase 2 — but the value is
///   never mutated after construction, so the one buffer we hold is cleared.
///
/// Held as bytes (not `String`) because a header field-value is a byte string
/// and because a plain byte buffer zeroizes without any `unsafe`.
pub struct ForwardedHeaderValue(Vec<u8>);

impl ForwardedHeaderValue {
    /// Wrap `bytes` as a forwarded header value, rejecting control characters
    /// (CR/LF/NUL and friends — the header-injection defense; HTAB is allowed)
    /// and anything over [`MAX_HEADER_VALUE_LEN`]. An empty value is allowed:
    /// some legitimate headers are empty, and emptiness carries no secret.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, HeaderError> {
        let bytes = bytes.into();
        Self::validate(&bytes)?;
        Ok(ForwardedHeaderValue(bytes))
    }

    /// The value-acceptance rules ([`Self::new`]'s checks) applied to a borrowed
    /// slice, without allocating or wrapping. A value **backend** validates with
    /// this at its `set` boundary so a value that could never be read back
    /// (oversize or control-char-bearing — `get` rejects it via [`Self::new`])
    /// is refused at entry time instead of silently failing at forward time.
    pub fn validate(bytes: &[u8]) -> Result<(), HeaderError> {
        if bytes.len() > MAX_HEADER_VALUE_LEN {
            return Err(HeaderError::ValueTooLong {
                len: bytes.len(),
                max: MAX_HEADER_VALUE_LEN,
            });
        }
        if bytes.iter().any(|&b| is_forbidden_value_byte(b)) {
            return Err(HeaderError::ControlCharInValue);
        }
        Ok(())
    }

    /// The value's byte length. Safe to log — a length is not a secret.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Read the raw value bytes. **This is the exposure boundary** — call it
    /// only where the value is injected into a downstream request, never to
    /// build a log, error, event, or audit record. Every call site is an
    /// auditable point where a secret leaves the wrapper.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

/// Redacted — never prints the value, only that it is one and its length.
impl fmt::Debug for ForwardedHeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ForwardedHeaderValue(<redacted; {} bytes>)",
            self.0.len()
        )
    }
}

/// Redacted — a bare marker, so string interpolation can't leak the value.
impl fmt::Display for ForwardedHeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Best-effort scrub of the backing buffer on drop.
impl Drop for ForwardedHeaderValue {
    fn drop(&mut self) {
        zeroize_vec(&mut self.0);
    }
}

/// Best-effort scrub of a `Vec<u8>`'s **entire allocation**. Two things a naive
/// `for b in &mut v { *b = 0 }` gets wrong for secret material:
///
/// - it only touches `len()` bytes, leaving `capacity() - len()` spare-capacity
///   bytes (which may still hold secret data from a prior larger value) in the
///   freed allocation — so we grow to capacity first (`resize` never
///   reallocates when the new length is ≤ capacity);
/// - a plain store can be dead-store-eliminated in optimized builds — so we
///   `write_volatile` every byte, which the compiler must not elide.
///
/// Shared by [`ForwardedHeaderValue`]'s drop and the in-memory secret backend.
pub(crate) fn zeroize_vec(buf: &mut Vec<u8>) {
    let cap = buf.capacity();
    buf.resize(cap, 0);
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, mutable reference for the duration
        // of this call — all `write_volatile` requires.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// Control bytes that must never appear in a forwarded header value. CR, LF and
/// NUL are the injection vectors; other C0 controls and DEL are refused too.
/// HTAB (`0x09`) is permitted (RFC 7230 field-value allows it).
fn is_forbidden_value_byte(b: u8) -> bool {
    (b < 0x20 && b != b'\t') || b == 0x7f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_name_canonicalizes_case_and_whitespace() {
        for raw in ["Authorization", "authorization", "  AUTHORIZATION  "] {
            assert_eq!(HeaderName::parse(raw).unwrap().as_str(), "authorization");
        }
    }

    #[test]
    fn case_variants_are_the_same_name() {
        // The anti-smuggling property: duplicates that differ only in case
        // collapse to one name, so a set/map can't hold two `Authorization`s.
        assert_eq!(
            HeaderName::parse("Authorization").unwrap(),
            HeaderName::parse("AUTHORIZATION").unwrap(),
        );
    }

    #[test]
    fn empty_and_malformed_names_are_rejected() {
        assert_eq!(
            HeaderName::parse("   ").unwrap_err(),
            HeaderError::EmptyName
        );
        // A colon, space, or newline are not token characters.
        for bad in ["a b", "x:y", "with\nnewline", "café"] {
            assert!(
                matches!(HeaderName::parse(bad), Err(HeaderError::InvalidName { .. })),
                "{bad:?} must be rejected",
            );
        }
    }

    #[test]
    fn classifies_sensitive_and_hop_by_hop() {
        let auth = HeaderName::parse("Authorization").unwrap();
        assert!(auth.is_security_sensitive());
        assert!(auth.is_forwardable(), "sensitive but end-to-end");

        for hop in [
            "Connection",
            "Transfer-Encoding",
            "Proxy-Authorization",
            "keep-alive",
        ] {
            let h = HeaderName::parse(hop).unwrap();
            assert!(h.is_hop_by_hop(), "{hop:?} is hop-by-hop");
            assert!(!h.is_forwardable(), "{hop:?} must never forward");
        }

        let trace = HeaderName::parse("X-Trace-Id").unwrap();
        assert!(!trace.is_security_sensitive());
        assert!(trace.is_forwardable());
    }

    #[test]
    fn widened_credential_classification() {
        // Vendor bearer-credential headers are sensitive (exact + substring), so
        // they can't ride the plain path or evade the credential risk tag.
        for cred in [
            "x-api-key",
            "X-Api-Key",
            "x-acme-api-key",
            "x-auth-token",
            "x-amz-security-token",
            "x-session-token",
            "x-vault-secret",
            "proxy-authorization",
            "x-functions-key",
        ] {
            assert!(
                HeaderName::parse(cred).unwrap().is_security_sensitive(),
                "{cred:?} must be treated as a credential",
            );
        }
        // Benign `*-key` / `*-token` headers stay plain-eligible — they carry no
        // long-lived secret, and the narrow heuristic deliberately excludes them.
        for plain in [
            "x-trace-id",
            "x-tenant-id",
            "x-idempotency-key",
            "x-partition-key",
            "x-csrf-token",
            "x-request-id",
        ] {
            assert!(
                !HeaderName::parse(plain).unwrap().is_security_sensitive(),
                "{plain:?} must stay plain-eligible",
            );
        }
    }

    #[test]
    fn name_over_the_length_cap_is_rejected() {
        assert!(
            HeaderName::parse(&"x".repeat(MAX_HEADER_NAME_LEN)).is_ok(),
            "a name at the cap is fine",
        );
        assert!(matches!(
            HeaderName::parse(&"x".repeat(MAX_HEADER_NAME_LEN + 1)).unwrap_err(),
            HeaderError::NameTooLong { .. },
        ));
    }

    #[test]
    fn value_rejects_control_chars_and_oversize() {
        assert!(ForwardedHeaderValue::new(b"Bearer abc123".to_vec()).is_ok());
        // Tab is allowed; CR/LF/NUL are not.
        assert!(ForwardedHeaderValue::new(b"a\tb".to_vec()).is_ok());
        for bad in [b"a\r\nInjected: x".to_vec(), b"a\0b".to_vec(), vec![0x1b]] {
            assert_eq!(
                ForwardedHeaderValue::new(bad).unwrap_err(),
                HeaderError::ControlCharInValue,
            );
        }
        let too_long = vec![b'x'; MAX_HEADER_VALUE_LEN + 1];
        assert!(matches!(
            ForwardedHeaderValue::new(too_long).unwrap_err(),
            HeaderError::ValueTooLong { .. },
        ));
    }

    #[test]
    fn validate_matches_new_without_allocating() {
        // The borrow-only entry check a backend uses at `set` must agree with
        // `new` (what `get` runs), so a value accepted at entry always reads back.
        assert!(ForwardedHeaderValue::validate(b"Bearer abc123").is_ok());
        assert!(ForwardedHeaderValue::validate(b"a\tb").is_ok());
        assert_eq!(
            ForwardedHeaderValue::validate(b"a\r\nInjected: x").unwrap_err(),
            HeaderError::ControlCharInValue,
        );
        assert!(matches!(
            ForwardedHeaderValue::validate(&vec![b'x'; MAX_HEADER_VALUE_LEN + 1]).unwrap_err(),
            HeaderError::ValueTooLong { .. },
        ));
    }

    #[test]
    fn value_never_prints_its_contents() {
        let v = ForwardedHeaderValue::new(b"ghp_SUPERSECRET".to_vec()).unwrap();
        let dbg = format!("{v:?}");
        let disp = format!("{v}");
        assert!(
            !dbg.contains("SUPERSECRET"),
            "Debug leaked the value: {dbg}"
        );
        assert!(!disp.contains("SUPERSECRET"), "Display leaked the value");
        assert!(dbg.contains("redacted"));
        assert_eq!(disp, "<redacted>");
        // The value is only reachable through the explicit exposure boundary.
        assert_eq!(v.expose(), b"ghp_SUPERSECRET");
    }

    #[test]
    fn zeroize_vec_scrubs_the_full_capacity() {
        // Reserve more than we fill, so there is spare capacity to cover.
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(b"secret");
        zeroize_vec(&mut v);
        assert_eq!(
            v.len(),
            16,
            "grown to capacity so spare bytes are scrubbed too"
        );
        assert!(v.iter().all(|&b| b == 0), "every byte is zero");
    }
}
