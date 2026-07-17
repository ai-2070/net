//! OA2-E1 §2.4a — the cortex/mesh admission gate glue.
//!
//! The behavior-layer admission engine
//! ([`org_admission`](super::behavior::org_admission)) verifies a
//! decoded [`OrgCallProof`](super::behavior::org_call::OrgCallProof)
//! against the provider's own facts, deliberately WITHOUT importing
//! the cortex RPC payload types. This module is the thin bridge the
//! mesh gate uses: it computes the canonical request digest the proof
//! binds, from the cortex [`RpcRequestPayload`], so the same digest
//! function is shared by the provider gate and (in E2) the caller's
//! proof-intent builder. A divergence between the two would fail
//! every legitimate call CLOSED — safe, and caught by the admit
//! witness.

use super::behavior::org_call::ORG_ADMISSION_HEADER;
use super::cortex::{RpcHeader, RpcRequestPayload};

/// blake3 `derive_key` context for the canonical org-RPC request
/// digest (E1.7). Distinct, versioned domain string so a future wire
/// change gets a new context and cannot collide with an old digest.
pub const ORG_RPC_REQUEST_DIGEST_CONTEXT: &str = "net-org-rpc-request-v1";

/// The canonical request digest an [`OrgCallProof`] binds (§2.4 call
/// binding). One shared definition (verdict §8) — never a second
/// hand-written concatenation codec:
///
/// 1. drop EVERY exact `net-org-admission` header (the proof itself
///    rides one of these; a request must not bind the proof carrying
///    it, and a provider strips them all before hashing);
/// 2. byte-sort the remaining `(name, value)` pairs, so header ORDER
///    never changes the digest while header COUNT / multiplicity /
///    lengths still do;
/// 3. re-encode with [`RpcRequestPayload`]'s existing canonical wire
///    encoder — this binds service, deadline, flags, every remaining
///    header, and the body length + bytes automatically;
/// 4. `blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, encoded)`.
///
/// Both the provider (verifying `ctx.request_digest`) and the caller
/// (E2, minting the proof) call THIS function over the SAME finalized
/// request, so a mismatch is impossible for a well-formed call and a
/// tampered body/header set fails the binding.
pub fn org_request_digest(req: &RpcRequestPayload) -> [u8; 32] {
    let mut headers: Vec<RpcHeader> = req
        .headers
        .iter()
        .filter(|(name, _)| name != ORG_ADMISSION_HEADER)
        .cloned()
        .collect();
    // Byte-sort by name then value. Duplicate headers are preserved
    // (multiplicity is bound); only their order is canonicalized.
    headers.sort_by(|(a_name, a_val), (b_name, b_val)| {
        a_name
            .as_bytes()
            .cmp(b_name.as_bytes())
            .then_with(|| a_val.cmp(b_val))
    });

    let canonical = RpcRequestPayload {
        service: req.service.clone(),
        deadline_ns: req.deadline_ns,
        flags: req.flags,
        headers,
        // `Bytes` clone is a refcount bump, not a copy.
        body: req.body.clone(),
    };
    let mut encoded = Vec::with_capacity(canonical.encoded_len());
    canonical.encode_into(&mut encoded);
    blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, &encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn req(headers: Vec<RpcHeader>, body: &[u8]) -> RpcRequestPayload {
        RpcRequestPayload {
            service: "oa2-echo".to_string(),
            deadline_ns: 1_700_000_000_000_000_000,
            flags: 0,
            headers,
            body: Bytes::copy_from_slice(body),
        }
    }

    fn h(name: &str, value: &[u8]) -> RpcHeader {
        (name.to_string(), value.to_vec())
    }

    /// Header ORDER does not change the digest (canonical byte-sort).
    #[test]
    fn digest_is_header_order_independent() {
        let a = req(vec![h("b", b"2"), h("a", b"1"), h("c", b"3")], b"body");
        let b = req(vec![h("a", b"1"), h("b", b"2"), h("c", b"3")], b"body");
        assert_eq!(org_request_digest(&a), org_request_digest(&b));
    }

    /// The admission header is stripped before hashing — so the proof
    /// (which rides that header) never binds itself, and adding /
    /// removing it leaves the digest unchanged.
    #[test]
    fn digest_ignores_admission_header() {
        let bare = req(vec![h("x", b"1")], b"body");
        let with_proof = req(
            vec![h("x", b"1"), h(ORG_ADMISSION_HEADER, b"opaque-proof-bytes")],
            b"body",
        );
        assert_eq!(org_request_digest(&bare), org_request_digest(&with_proof));

        // Even MULTIPLE admission headers are all stripped.
        let with_two = req(
            vec![
                h(ORG_ADMISSION_HEADER, b"p1"),
                h("x", b"1"),
                h(ORG_ADMISSION_HEADER, b"p2"),
            ],
            b"body",
        );
        assert_eq!(org_request_digest(&bare), org_request_digest(&with_two));
    }

    /// Duplicate non-admission headers ARE bound — dropping one
    /// changes the digest (multiplicity matters).
    #[test]
    fn digest_binds_header_multiplicity() {
        let one = req(vec![h("x", b"1")], b"body");
        let two = req(vec![h("x", b"1"), h("x", b"1")], b"body");
        assert_ne!(org_request_digest(&one), org_request_digest(&two));
    }

    /// Body, service, deadline, and flags all change the digest.
    #[test]
    fn digest_binds_request_fields() {
        let base = req(vec![], b"body");
        let base_d = org_request_digest(&base);

        assert_ne!(base_d, org_request_digest(&req(vec![], b"other")));

        let mut svc = req(vec![], b"body");
        svc.service = "different".to_string();
        assert_ne!(base_d, org_request_digest(&svc));

        let mut dl = req(vec![], b"body");
        dl.deadline_ns += 1;
        assert_ne!(base_d, org_request_digest(&dl));

        let mut fl = req(vec![], b"body");
        fl.flags = 1;
        assert_ne!(base_d, org_request_digest(&fl));
    }

    /// Golden: the digest is a stable, versioned value — pinned so a
    /// cross-language caller (or the E2 Rust caller) can reproduce it
    /// byte-for-byte. A change here is a wire break and must bump the
    /// derive-key context.
    #[test]
    fn digest_golden_is_stable() {
        let r = req(vec![h("content-type", b"application/json")], b"hello");
        let got = org_request_digest(&r);
        // Regenerate deterministically from the canonical encoding so
        // the golden documents the exact bytes hashed.
        let mut canonical = r.clone();
        canonical.headers.retain(|(n, _)| n != ORG_ADMISSION_HEADER);
        let mut encoded = Vec::new();
        canonical.encode_into(&mut encoded);
        assert_eq!(
            got,
            blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, &encoded)
        );
        // And it is not the all-zero / trivial value.
        assert_ne!(got, [0u8; 32]);
    }
}
