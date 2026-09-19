//! The A2A task admission seam: the provider gate and the caller's
//! evidence for **paid agent-to-agent tasks** served straight from the
//! SDK — the twin of [`tool_payment`](crate::tool_payment), one step
//! further from the money.
//!
//! The invariant this module completes: **a paid task is admitted
//! before it runs, and one payment admits exactly one reservation of
//! exactly one brief.** A paid A2A service reserves capacity and mints
//! an admission id *before* a quote exists; the caller's quote commits
//! to that reservation through its input hash; and
//! [`TaskAdmissionGate::redeem`] refuses any proof whose purchase hash
//! isn't the one this provider is expecting — so the same payment
//! replayed under another owner, another reservation, or another brief
//! buys nothing.
//!
//! The wire vocabulary is shared, not re-invented: the quote id rides
//! [`HDR_PAYMENT_QUOTE`](crate::tool_payment::HDR_PAYMENT_QUOTE), the
//! possession proof rides
//! [`HDR_PAYMENT_BINDING`](crate::tool_payment::HDR_PAYMENT_BINDING) —
//! **mandatory** here, where a paid task is a long-running side effect
//! rather than one invocation, so bearer presentation is never enough —
//! and a refusal is the application error
//! [`ERR_PAYMENT`](crate::tool_payment::ERR_PAYMENT) carrying a
//! [`FailureSchematic`](crate::tool_payment::FailureSchematic) beside
//! the human message. A gate denial is the same
//! [`GateDenial`] both seams speak.
//!
//! The SDK never verifies payments itself — it holds no payment state
//! and parses no payment objects. The gate is the seam: `net-payments`
//! implements it over its `PaymentEngine` (settled, billed, unfrozen,
//! bound to this task's purchase hash), and tests script it.
//!
//! **Ungated on purpose**, exactly like `tool_payment`: an implementor
//! of the gate needs these four shapes and nothing else, and a free A2A
//! service must never link a payments crate to serve work it gives
//! away. (`Mesh::serve_a2a` and friends are named as plain spans rather
//! than intra-doc links for the same reason — they live behind the
//! `net`/`cortex` features and a link would dangle under
//! `--no-default-features`.)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool_payment::GateDenial;

/// Service name of the uncharged prepare verb: validate a brief,
/// reserve capacity, and mint the admission id a purchase binds to.
/// Read-only on the money side — nothing here can charge.
pub const A2A_PREPARE_SERVICE: &str = "net.a2a.prepare";

/// Service name of the uncharged discovery verb: what this node serves,
/// what it accepts, and what it costs.
pub const A2A_DESCRIBE_SERVICE: &str = "net.a2a.describe";

/// One task admission presented to the gate.
///
/// `expected_input_hash` is the provider's own `purchase_hash` (the
/// `a2a` module's — named as a plain span because that module is gated
/// behind the `net` feature and a link would dangle in an ungated
/// build) for the reservation the submission arrived against, never a
/// value read off the request. The gate's job is to refuse unless the
/// quote was issued for *that* hash, which is what makes a valid
/// payment for one reservation worthless against another.
#[derive(Debug, Clone, Copy)]
pub struct TaskPaymentClaim<'a> {
    /// The paid task's tool id, `"net.a2a.task/{service_id}"`.
    pub tool_id: &'a str,
    /// The quote id the caller presented.
    pub quote_id: &'a str,
    /// The caller's signature over the invocation-binding transcript.
    /// Mandatory: a task admission never falls back to bearer.
    pub binding: &'a [u8],
    /// The purchase hash this provider expects the quote to commit to.
    pub expected_input_hash: &'a str,
}

/// What the gate hands back when a task admission is paid for: the quote
/// that paid, and who paid it.
///
/// `payer` is the verified payer entity — a serving path with a verified
/// end-to-end principal (organization admission) matches it against the
/// admitted caller, so a payment made by one entity cannot admit
/// another's task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPaymentEvidence {
    /// The redeemed quote.
    pub quote_id: String,
    /// The entity whose payment this is.
    pub payer: [u8; 32],
}

/// The provider-side admission gate for paid A2A tasks: redeem a paid
/// quote for one reservation of one task.
///
/// `Err(denial)` refuses the submission — the denial's `message` travels
/// to the caller as the body of the `ERR_PAYMENT` application error and
/// its `schematic` rides the failure-schematic header, byte-identical to
/// the paid-tool path. Nothing is launched, and the reservation survives
/// the refusal: a denial is an attempt note, not a state change, so a
/// caller that fixes its payment retries the same admission instead of
/// preparing (and paying for) a second one.
///
/// This is the task twin of [`ToolPaymentGate`](crate::tool_payment::ToolPaymentGate);
/// `net-payments` provides the engine-backed implementation and the
/// single denial-render site.
#[async_trait]
pub trait TaskAdmissionGate: Send + Sync {
    /// Redeem `claim`, or refuse it.
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial>;
}

/// The caller's transport evidence for a paid task: what to present at
/// submit.
///
/// Deliberately thin — the quote id and the binding signature are what
/// the wire carries. Billing artifacts and the durable purchase attempt
/// stay on the caller's own records; this type is the part that travels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPaymentProof {
    /// The quote the caller paid.
    pub quote_id: String,
    /// The caller's signature over the invocation-binding transcript.
    pub binding_sig: Vec<u8>,
}
