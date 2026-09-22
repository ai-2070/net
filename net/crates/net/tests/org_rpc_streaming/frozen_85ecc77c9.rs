//! The FROZEN old provider at revision `85ecc77c9`, vendored for
//! `frozen_old_provider_refuses_stream_proof_with_not_supported`
//! (`ORG_SCOPED_STREAMING_PLAN.md` §1.4): the `OrgCallProof` type,
//! `verify_org_admission` and `serve_rpc_protected` AS THEY EXISTED at that
//! revision — never the new implementation configured to behave like one.
//!
//! Layout:
//!
//! * [`old_org_call`] — the frozen proof + `"net-org-call-v1"` transcript +
//!   prefix-tolerant decoder (`org_call.rs:50-359` @`85ecc77c9`, verbatim);
//! * [`old_org_admission`] — the frozen `verify_org_admission` with its
//!   `is_unary` step 4 and the frozen denial/coarse types
//!   (`org_admission.rs:64-662`, verbatim);
//! * [`old_serve`] — the frozen `serve_rpc_protected` (`mesh_rpc.rs:3400-3438`,
//!   verbatim) plus the frozen flag derivation (`:1124-1127`) and denial wire
//!   shape (`:872-876`), and the two named shim seams its body needs.
//!
//! Each module's doc lists its extraction command, extraction sha256, and
//! every adaptation line. See also the runner [`frozen_opening`] in
//! [`old_serve`].

// `#[rustfmt::skip]` on each vendored module: their bodies are byte-identical
// to the recorded `85ecc77c9` extractions (sha256 in each file's doc), and a
// formatting pass would silently destroy that provenance.
#[path = "frozen_85ecc77c9/old_org_admission.rs"]
#[rustfmt::skip]
pub mod old_org_admission;
#[path = "frozen_85ecc77c9/old_org_call.rs"]
#[rustfmt::skip]
pub mod old_org_call;
#[path = "frozen_85ecc77c9/old_serve.rs"]
#[rustfmt::skip]
pub mod old_serve;

pub use old_serve::{frozen_opening, FrozenProvider, FrozenYield, UnaryAdmission};
