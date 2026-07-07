//! Scheme-specific payload authoring — the caller side of "scheme-per-
//! chain". Pure document builders: no keys, no signatures, no I/O.
//! Signing happens behind [`crate::flow::signer::SchemeSigner`], and the
//! only crypto that can ever run inside Net is the dev signer's,
//! feature-gated.
//!
//! # Adding a scheme (the seam inventory)
//!
//! `exact` is the only scheme family built today, and the only one with a
//! pinned spec shape at the pinned x402 commit. A new scheme (`upto`,
//! RFQ/dynamic pricing, …) is **deliberately deferred** until its entry
//! criteria hold; until then an accepts[] entry with an unknown scheme
//! fails closed at selection (no settleable entry → structured `Denied`)
//! — a pinned contract, not an accident (see
//! `an_unknown_scheme_accepts_entry_fails_closed_at_selection`).
//!
//! **Entry criteria to unshelve a scheme:**
//!
//! 1. The scheme's spec is pinned at a commit (never a vendor-defined
//!    shape — the xrpl lesson, see `PAYMENTS_P1_NETWORK_LADDER.md`);
//! 2. a live facilitator advertises the `(scheme, network)` kind in its
//!    `GET /supported`;
//! 3. the scheme's **amount policy is reviewed as a money-path decision**
//!    — e.g. `upto` flips the engine's `Ordering::Greater` completion arm
//!    from `Exception{Overpayment}` (manual provider policy) to
//!    serve-at-delivered. That is a change to what the machine may bill
//!    without a human, and it goes to review, not into a code detail.
//!
//! **The seams a new scheme instantiates** (every one exists; none may
//! change shape for one scheme):
//!
//! - a `schemes/<name>.rs` authoring module: typed intent in, payload
//!   object out — no raw signing, no keys, no I/O (this module's rule);
//! - a [`crate::flow::signer::SchemeSigner`] operation (defaulted to a
//!   structured refusal so mismatched signers fail closed);
//! - the `can_settle` dispatch arms — **both** of them, the mesh flow
//!   (`flow/mod.rs`) and the HTTP door (`flow/http402.rs`), kept
//!   symmetric (P2 WS-B made them so; do not let them drift);
//! - replay identity: what is this scheme's nonce/authorization for the
//!   engine's canonical-payload `consumed` index (M2)? A scheme whose
//!   retries legitimately re-encode (exact-SVM's fresh blockhash) keys
//!   idempotency at the quote, not payload bytes — say which;
//! - checker delivered-amount semantics: how the independent checker
//!   reads "amount delivered to payTo, funded by the payer" for this
//!   scheme's settlement shape (H3 payer binding is non-negotiable);
//! - registry/pack entries: the asset allowlist and facilitator config
//!   pack rungs (config, not code).

pub mod exact_evm;
pub mod exact_svm;
