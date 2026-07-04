//! The `net.invoke.forwarded_context@1` object
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 0).
//!
//! A forwarded context carries caller-supplied headers (bearer tokens and
//! non-secret headers alike) to a destination capability. Publicly it is the
//! "Forwarded Invocation Context"; the header values are **authority
//! metadata, never capability input** — they appear in no tool schema,
//! argument, or result. This module defines the object's shape, its
//! **canonical associated-data (AAD) encoding**, and its validation. It does
//! **not** seal or ship anything — the AEAD sealing that binds this AAD, and
//! the caller-side injection, land in Phase 2.
//!
//! ## What the AAD binds, and why
//!
//! The sealed payload (the header values) is bound — via the AEAD's associated
//! data — to the non-secret envelope fields: destination node id, caller
//! origin, capability id, invocation id, issue time, expiry, the single-use
//! nonce, and the declared header names. [`ForwardedContext::canonical_aad`] is the exact,
//! deterministic byte string that binding uses. Because the header **values**
//! never enter the AAD, a captured blob reveals nothing, and because every
//! other field does, a captured blob can't be replayed against another
//! destination, caller, capability, or invocation — and dies at the TTL
//! regardless. (The AEAD's own nonce is derived per-seal from the sealed-box
//! ephemeral key, so the object's `nonce` field is an authenticated
//! uniqueness token bound in the AAD, not the AEAD nonce itself.)
//!
//! ## Declared vs sealed
//!
//! [`ForwardedContext::declared_names`] is the authenticated, in-the-clear
//! list of header names; the header **values** live in the private sealed map.
//! [`ForwardedContext::validate`] enforces that the two agree — the check that
//! catches a tampered or malformed context where the announced names don't
//! match the sealed contents.

use std::collections::{BTreeMap, BTreeSet};

use super::header::{
    ForwardedHeaderValue, HeaderName, MAX_FORWARDED_HEADERS, MAX_TOTAL_FORWARDED_BYTES,
};
use super::{MAX_TTL_SECS, OBJECT_TAG};

/// Version of the canonical AAD encoding scheme. Stamped as the first byte so
/// a future layout change is unambiguous rather than a silent reinterpretation
/// (the same discipline as `IDENTITY_ENVELOPE_VERSION`).
const AAD_SCHEME_VERSION: u8 = 1;

/// Why a [`ForwardedContext`] failed validation. No variant carries a header
/// **value** — only names, counts, and lengths, all of which are safe to
/// surface in a structured error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    /// One of the required identity fields (destination, caller, capability,
    /// invocation) was empty.
    #[error("forwarded context is missing a required field: {field}")]
    MissingField {
        /// Which field was empty.
        field: &'static str,
    },
    /// The context carried no headers — an empty forward is meaningless and is
    /// refused rather than sealed.
    #[error("forwarded context carries no headers")]
    NoHeaders,
    /// More than [`MAX_FORWARDED_HEADERS`] headers.
    #[error("forwarded context carries {count} headers, over the {max} limit")]
    TooManyHeaders {
        /// The offending count.
        count: usize,
        /// The configured limit.
        max: usize,
    },
    /// Combined header-value bytes exceeded [`MAX_TOTAL_FORWARDED_BYTES`].
    #[error("forwarded header values total {bytes} bytes, over the {max} limit")]
    TotalTooLarge {
        /// The offending total.
        bytes: usize,
        /// The configured limit.
        max: usize,
    },
    /// A hop-by-hop header (e.g. `Connection`, `Proxy-*`) appeared — these are
    /// never forwardable end-to-end.
    #[error("hop-by-hop header {name:?} cannot be forwarded")]
    HopByHopHeader {
        /// The offending name (safe to surface).
        name: String,
    },
    /// The declared header-name list did not match the sealed header map —
    /// a sign of tampering or a malformed context.
    #[error("declared header names do not match the sealed header set")]
    DeclaredMismatch,
    /// `expires_at` was not strictly after `issued_at`.
    #[error("forwarded context expiry {expires_at} is not after issue {issued_at}")]
    InvalidTtl {
        /// Unix-seconds issue time.
        issued_at: u64,
        /// Unix-seconds expiry.
        expires_at: u64,
    },
    /// The TTL exceeded [`MAX_TTL_SECS`]. Forwarded bearer material is never
    /// meant to be valid at rest, so a long TTL is refused.
    #[error("forwarded context TTL {ttl}s exceeds the {max}s maximum")]
    TtlTooLong {
        /// The requested TTL in seconds.
        ttl: u64,
        /// The configured cap.
        max: u64,
    },
}

/// The `net.invoke.forwarded_context@1` object.
///
/// The non-secret envelope fields are public; the header **values** live in a
/// private [`BTreeMap`] reachable only through [`Self::header`] /
/// [`Self::headers`] so they can't be serialized by accident. Construct with
/// [`Self::new`], then [`Self::validate`] before use.
#[derive(Debug)]
pub struct ForwardedContext {
    /// Destination node id the payload is sealed to.
    pub sealed_to: String,
    /// The AEAD-verified caller origin this context is issued for.
    pub caller_origin: String,
    /// The capability id the forward authorizes.
    pub capability_id: String,
    /// The specific invocation this context is bound to (replay defense).
    pub invocation_id: String,
    /// The declared, in-the-clear header names. Must equal the sealed map's
    /// keys (enforced by [`Self::validate`]).
    pub declared_names: BTreeSet<HeaderName>,
    /// Unix-seconds issue time.
    pub issued_at: u64,
    /// Unix-seconds expiry — short by default (`DEFAULT_TTL_SECS`).
    pub expires_at: u64,
    /// Single-use nonce (becomes the AEAD nonce in Phase 2; not in the AAD).
    pub nonce: [u8; 16],
    /// The sealed payload: header name → secret value. Private so it can never
    /// be serialized or logged through the struct.
    headers: BTreeMap<HeaderName, ForwardedHeaderValue>,
}

impl ForwardedContext {
    /// Build a context. `declared_names` is derived from the header map, so the
    /// invariant `declared == sealed` holds by construction; [`Self::validate`]
    /// still checks it (a later phase builds contexts where the declared names
    /// come from the authenticated AAD and the values from the decrypted
    /// payload, and those two paths must agree).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sealed_to: impl Into<String>,
        caller_origin: impl Into<String>,
        capability_id: impl Into<String>,
        invocation_id: impl Into<String>,
        issued_at: u64,
        expires_at: u64,
        nonce: [u8; 16],
        headers: BTreeMap<HeaderName, ForwardedHeaderValue>,
    ) -> Self {
        let declared_names = headers.keys().cloned().collect();
        Self {
            sealed_to: sealed_to.into(),
            caller_origin: caller_origin.into(),
            capability_id: capability_id.into(),
            invocation_id: invocation_id.into(),
            declared_names,
            issued_at,
            expires_at,
            nonce,
            headers,
        }
    }

    /// Look up a forwarded header value by (canonical) name.
    pub fn header(&self, name: &HeaderName) -> Option<&ForwardedHeaderValue> {
        self.headers.get(name)
    }

    /// Iterate the sealed headers. Used only at the injection boundary.
    pub fn headers(&self) -> impl Iterator<Item = (&HeaderName, &ForwardedHeaderValue)> {
        self.headers.iter()
    }

    /// The context's TTL in seconds (`expires_at - issued_at`), saturating at 0.
    pub fn ttl_secs(&self) -> u64 {
        self.expires_at.saturating_sub(self.issued_at)
    }

    /// Validate every structural invariant. Callers **must** run this before
    /// sealing or trusting a context; it is the single choke point for the
    /// object's rules (deny-wins — the first broken invariant is returned).
    pub fn validate(&self) -> Result<(), ContextError> {
        for (field, value) in [
            ("sealed_to", &self.sealed_to),
            ("caller_origin", &self.caller_origin),
            ("capability_id", &self.capability_id),
            ("invocation_id", &self.invocation_id),
        ] {
            if value.trim().is_empty() {
                return Err(ContextError::MissingField { field });
            }
        }

        if self.headers.is_empty() {
            return Err(ContextError::NoHeaders);
        }
        if self.headers.len() > MAX_FORWARDED_HEADERS {
            return Err(ContextError::TooManyHeaders {
                count: self.headers.len(),
                max: MAX_FORWARDED_HEADERS,
            });
        }

        let total: usize = self.headers.values().map(ForwardedHeaderValue::len).sum();
        if total > MAX_TOTAL_FORWARDED_BYTES {
            return Err(ContextError::TotalTooLarge {
                bytes: total,
                max: MAX_TOTAL_FORWARDED_BYTES,
            });
        }

        for name in self.headers.keys() {
            if name.is_hop_by_hop() {
                return Err(ContextError::HopByHopHeader {
                    name: name.as_str().to_string(),
                });
            }
        }

        // Declared names must exactly match the sealed keys. `BTreeMap` keys
        // are unique and canonicalized, so this also guarantees no
        // case-varied duplicate of a security-sensitive header slipped in.
        let sealed_keys: BTreeSet<HeaderName> = self.headers.keys().cloned().collect();
        if self.declared_names != sealed_keys {
            return Err(ContextError::DeclaredMismatch);
        }

        if self.expires_at <= self.issued_at {
            return Err(ContextError::InvalidTtl {
                issued_at: self.issued_at,
                expires_at: self.expires_at,
            });
        }
        let ttl = self.ttl_secs();
        if ttl > MAX_TTL_SECS {
            return Err(ContextError::TtlTooLong {
                ttl,
                max: MAX_TTL_SECS,
            });
        }

        Ok(())
    }

    /// The canonical associated-data byte string the AEAD binds the sealed
    /// payload to (Phase 2). Deterministic and self-describing:
    ///
    /// ```text
    /// scheme_version : 1 byte  = AAD_SCHEME_VERSION
    /// object_tag     : field   = "net.invoke.forwarded_context@1"
    /// sealed_to      : field
    /// caller_origin  : field
    /// capability_id  : field
    /// invocation_id  : field
    /// issued_at      : 8 bytes  (u64 big-endian, unix seconds)
    /// expires_at     : 8 bytes  (u64 big-endian, unix seconds)
    /// nonce          : 16 bytes (fixed)
    /// header_count   : 4 bytes  (u32 big-endian)
    /// header_names   : field × header_count, ascending canonical order
    /// ```
    ///
    /// where each `field` is a 4-byte big-endian length prefix followed by the
    /// raw bytes. Length-prefixing every variable field makes the encoding
    /// unambiguous (no delimiter can be smuggled), and leading with the object
    /// tag domain-separates this AAD from every other Net object. Header names
    /// come from [`Self::declared_names`], which is a sorted [`BTreeSet`], so
    /// the order is stable regardless of insertion order.
    pub fn canonical_aad(&self) -> Vec<u8> {
        canonical_aad_bytes(
            &self.sealed_to,
            &self.caller_origin,
            &self.capability_id,
            &self.invocation_id,
            self.issued_at,
            self.expires_at,
            &self.nonce,
            &self.declared_names,
        )
    }
}

/// The canonical AAD encoding, factored out so the sealed wire form
/// ([`super::SealedContext`]) reconstructs the *identical* bytes from its
/// cleartext envelope — one layout, one golden vector. See
/// [`ForwardedContext::canonical_aad`] for the documented field order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn canonical_aad_bytes(
    sealed_to: &str,
    caller_origin: &str,
    capability_id: &str,
    invocation_id: &str,
    issued_at: u64,
    expires_at: u64,
    nonce: &[u8; 16],
    declared_names: &BTreeSet<HeaderName>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(AAD_SCHEME_VERSION);
    write_field(&mut out, OBJECT_TAG.as_bytes());
    write_field(&mut out, sealed_to.as_bytes());
    write_field(&mut out, caller_origin.as_bytes());
    write_field(&mut out, capability_id.as_bytes());
    write_field(&mut out, invocation_id.as_bytes());
    out.extend_from_slice(&issued_at.to_be_bytes());
    out.extend_from_slice(&expires_at.to_be_bytes());
    out.extend_from_slice(nonce);
    // `declared_names.len()` fits u32 by construction (MAX_FORWARDED_HEADERS).
    out.extend_from_slice(&(declared_names.len() as u32).to_be_bytes());
    for name in declared_names {
        write_field(&mut out, name.as_str().as_bytes());
    }
    out
}

/// Append a 4-byte big-endian length prefix followed by `bytes`.
#[expect(
    clippy::expect_used,
    reason = "AAD fields are capped far below u32::MAX (header names, node/caller/capability/invocation ids); a field that large is a logic bug, not runtime input — fail fast rather than emit a truncated, non-canonical length prefix"
)]
fn write_field(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).expect("AAD field length exceeds u32::MAX");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::DEFAULT_TTL_SECS;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    fn hv(s: &str) -> ForwardedHeaderValue {
        ForwardedHeaderValue::new(s.as_bytes().to_vec()).unwrap()
    }

    /// A fixed context used by the golden-vector test. Deliberately hand-built
    /// with stable field values so the AAD encoding is pinned byte-for-byte.
    fn golden_context() -> ForwardedContext {
        let mut headers = BTreeMap::new();
        headers.insert(
            HeaderName::parse("Authorization").unwrap(),
            hv("Bearer t0ken"),
        );
        headers.insert(HeaderName::parse("X-Tenant-Id").unwrap(), hv("acme"));
        ForwardedContext::new(
            "node-dest-01",
            "origin-caller-07",
            "github.create_issue",
            "inv-0000000000000001",
            1_000_000,
            1_000_000 + DEFAULT_TTL_SECS,
            [0u8; 16],
            headers,
        )
    }

    #[test]
    fn golden_context_is_valid() {
        golden_context().validate().unwrap();
    }

    #[test]
    fn canonical_aad_matches_golden_vector() {
        // Pins the AAD wire encoding. A deliberate layout change re-runs the
        // `emit_golden_aad` emitter below and pastes the new hex here.
        const GOLDEN_AAD_HEX: &str = "010000001e6e65742e696e766f6b652e666f727761726465645f636f6e7465787440310000000c6e6f64652d646573742d3031000000106f726967696e2d63616c6c65722d3037000000136769746875622e6372656174655f697373756500000014696e762d3030303030303030303030303030303100000000000f424000000000000f425e00000000000000000000000000000000000000020000000d617574686f72697a6174696f6e0000000b782d74656e616e742d6964";
        let aad = golden_context().canonical_aad();
        assert_eq!(to_hex(&aad), GOLDEN_AAD_HEX, "forwarded-context AAD drift");
    }

    /// Regenerate `GOLDEN_AAD_HEX`:
    /// `cargo test -p net-mesh-mcp --features fixture --lib \
    ///   forward::context::tests::emit_golden_aad -- --ignored --nocapture`
    #[test]
    #[ignore = "emitter for the golden AAD vector"]
    fn emit_golden_aad() {
        println!(
            "GOLDEN_AAD_HEX = {}",
            to_hex(&golden_context().canonical_aad())
        );
    }

    #[test]
    fn aad_excludes_values_but_binds_names_and_expiry() {
        let aad = golden_context().canonical_aad();
        // The header NAMES are bound...
        let text = String::from_utf8_lossy(&aad);
        assert!(text.contains("authorization"), "header names are bound");
        assert!(text.contains("x-tenant-id"));
        // ...but no VALUE is ever present.
        assert!(!text.contains("Bearer"), "values must never enter the AAD");
        assert!(!text.contains("t0ken"));
        // ...and the object tag domain-separates it.
        assert!(text.contains(OBJECT_TAG));
    }

    #[test]
    fn aad_changes_when_a_bound_field_changes() {
        let base = golden_context().canonical_aad();
        // Different destination → different AAD (replay-across-destination
        // defense is rooted here).
        let mut headers = BTreeMap::new();
        headers.insert(
            HeaderName::parse("Authorization").unwrap(),
            hv("Bearer t0ken"),
        );
        headers.insert(HeaderName::parse("X-Tenant-Id").unwrap(), hv("acme"));
        let other = ForwardedContext::new(
            "node-dest-99",
            "origin-caller-07",
            "github.create_issue",
            "inv-0000000000000001",
            1_000_000,
            1_000_000 + DEFAULT_TTL_SECS,
            [0u8; 16],
            headers,
        );
        assert_ne!(base, other.canonical_aad());
    }

    #[test]
    fn validate_rejects_missing_fields() {
        let mut headers = BTreeMap::new();
        headers.insert(HeaderName::parse("X-Trace-Id").unwrap(), hv("abc"));
        let ctx = ForwardedContext::new(
            "", // empty destination
            "caller", "cap", "inv", 1, 2, [0u8; 16], headers,
        );
        assert_eq!(
            ctx.validate().unwrap_err(),
            ContextError::MissingField { field: "sealed_to" },
        );
    }

    #[test]
    fn validate_rejects_hop_by_hop_header() {
        let mut headers = BTreeMap::new();
        headers.insert(HeaderName::parse("Connection").unwrap(), hv("keep-alive"));
        let ctx = ForwardedContext::new("dest", "caller", "cap", "inv", 1, 2, [0u8; 16], headers);
        assert!(matches!(
            ctx.validate().unwrap_err(),
            ContextError::HopByHopHeader { .. },
        ));
    }

    #[test]
    fn validate_rejects_declared_mismatch() {
        let ctx = golden_context();
        // Tamper: declare a name that isn't in the sealed set.
        let mut ctx = ctx;
        ctx.declared_names
            .insert(HeaderName::parse("X-Extra").unwrap());
        assert_eq!(ctx.validate().unwrap_err(), ContextError::DeclaredMismatch,);
    }

    #[test]
    fn validate_rejects_bad_ttl() {
        let mut headers = BTreeMap::new();
        headers.insert(HeaderName::parse("X-Trace-Id").unwrap(), hv("abc"));
        // expires_at not after issued_at.
        let ctx =
            ForwardedContext::new("dest", "caller", "cap", "inv", 100, 100, [0u8; 16], headers);
        assert!(matches!(
            ctx.validate().unwrap_err(),
            ContextError::InvalidTtl { .. },
        ));

        // TTL over the maximum.
        let mut headers = BTreeMap::new();
        headers.insert(HeaderName::parse("X-Trace-Id").unwrap(), hv("abc"));
        let ctx = ForwardedContext::new(
            "dest",
            "caller",
            "cap",
            "inv",
            0,
            MAX_TTL_SECS + 1,
            [0u8; 16],
            headers,
        );
        assert!(matches!(
            ctx.validate().unwrap_err(),
            ContextError::TtlTooLong { .. },
        ));
    }
}
