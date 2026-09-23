//! The canonical request digest an [`OrgCallProof`] binds.
//!
//! Core's `org_admission_gate.rs::org_request_digest`, verbatim
//! against the leaf's own [`RpcRequestPayload`] codec — one shared
//! definition (never a second hand-written concatenation codec):
//!
//! 1. drop EVERY exact `net-org-admission` header (the proof itself
//!    rides one of these; a request must not bind the proof carrying
//!    it, and a provider strips them all before hashing);
//! 2. PRESERVE the relative order of every remaining header;
//! 3. re-encode with [`RpcRequestPayload`]'s existing canonical wire
//!    encoder — this binds service, deadline, flags, every remaining
//!    header (in order, with multiplicity), and the body length +
//!    bytes automatically;
//! 4. `blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, encoded)`.
//!
//! Header ORDER is bound, NOT canonicalized away (Kyra E1 audit): the
//! application receives the original ordered headers and existing
//! parsers are order-sensitive (trace extraction is
//! last-duplicate-wins; stream-window parsing is first-duplicate-wins),
//! so the proof must bind the exact sequence the handler interprets.
//! Sorting here would let `[("x","allow"),("x","deny")]` and its
//! reverse sign the same digest while delivering different meaning.
//!
//! Both the provider (verifying `ctx.request_digest`) and the caller
//! (minting the proof) call THIS function over the SAME finalized
//! request, so a mismatch is impossible for a well-formed call and a
//! tampered body/header set/order fails the binding.

use crate::error::Result;
use crate::org::proof::ORG_ADMISSION_HEADER;
use crate::rpc_wire::{RpcHeader, RpcRequestPayload};

/// blake3 `derive_key` context for the canonical org-RPC request
/// digest (E1.7). Distinct, versioned domain string so a future wire
/// change gets a new context and cannot collide with an old digest.
pub const ORG_RPC_REQUEST_DIGEST_CONTEXT: &str = "net-org-rpc-request-v1";

/// The canonical request digest an
/// [`OrgCallProof`](crate::org::proof::OrgCallProof) binds (§2.4
/// call binding).
///
/// Validates the FINALIZED request — the exact bytes the caller
/// signs and the provider decodes, proof headers INCLUDED — before
/// stripping (R2-1: stripping first would let a structurally invalid
/// finalized request reduce to a valid canonical after the strip and
/// hash `Ok`, so the proof would bind a request no honest party could
/// have put on the wire), then validates the stripped canonical form
/// before encoding it.
pub fn org_request_digest(req: &RpcRequestPayload) -> Result<[u8; 32]> {
    req.validate()?;
    // Strip the admission headers ONLY; the relative order of every
    // other header is preserved exactly as the application will see
    // it.
    let headers: Vec<RpcHeader> = req
        .headers
        .iter()
        .filter(|(name, _)| name.as_str() != ORG_ADMISSION_HEADER)
        .cloned()
        .collect();

    let canonical = RpcRequestPayload {
        service: req.service.clone(),
        deadline_ns: req.deadline_ns,
        flags: req.flags,
        headers,
        // `Bytes` clone is a refcount bump, not a copy.
        body: req.body.clone(),
    };
    // Refuse an over-cap request rather than hash a
    // release-truncated, ambiguous encoding (`encode_into`'s length
    // prefixes are `as u8`/`as u16`/`as u32` casts): `encode_into`
    // re-validates the canonical form first.
    let mut encoded = Vec::with_capacity(canonical.encoded_len());
    canonical.encode_into(&mut encoded)?;
    Ok(blake3::derive_key(ORG_RPC_REQUEST_DIGEST_CONTEXT, &encoded))
}
