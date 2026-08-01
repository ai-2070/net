//! The two-way door, outbound: a Net agent paying an **external x402
//! HTTP API** — same objects, same spend policy, same signers, zero
//! translation.
//!
//! Wire mechanics per the pinned v2 HTTP transport (header-only; bodies
//! are the server's business):
//!
//! - `402` + `PAYMENT-REQUIRED` header: base64 `PaymentRequired` JSON;
//! - retry with `PAYMENT-SIGNATURE` header: base64 of *our* payload's
//!   preserved bytes;
//! - success carries `PAYMENT-RESPONSE`: base64 `SettlementResponse`,
//!   landed byte-preserved for audit.
//!
//! Policy runs on a **local pseudo-quote** over the chosen accepts
//! entry: there is no provider identity and no signed quote on this
//! path — the external server's demand is the commercial fact, and the
//! caller's own spend engine (caps, network enablement, approvals) is
//! the entire gate. The pseudo-quote's capability key is
//! `x402-http/<host>`, so per-capability overrides and approval
//! redemption work per external host.
//!
//! Honesty note on retries: HTTP has no provider-side idempotency here.
//! A lost response after a settled payment is a dispute with the
//! external server, not something this client can dedupe — one
//! `fetch_paid` call authors one payment attempt.

#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use super::signer::SchemeSigner;
use super::{exact_evm_authorization_for_quote, Clock};
use crate::core::quote::PaymentQuote;
use crate::core::registry::AssetRegistry;
use crate::policy::spend::{SpendDecision, SpendPolicyEngine};
use crate::x402::payload::PaymentPayload;
use crate::x402::payment_required::PaymentRequired;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::settlement::SettlementResponse;
use crate::x402::{X402Carry, X402_VERSION};
use net::adapter::net::identity::EntityKeypair;

/// Client → server payment payload header (v2 HTTP transport).
pub const HDR_PAYMENT_SIGNATURE: &str = "payment-signature";
/// Server → client payment demand header on 402.
pub const HDR_PAYMENT_REQUIRED: &str = "payment-required";
/// Server → client settlement response header on success.
pub const HDR_PAYMENT_RESPONSE: &str = "payment-response";

/// The structured outcome of a paid HTTP fetch.
#[derive(Debug)]
pub enum X402HttpOutcome {
    /// The resource needed no payment (or the server answered without a
    /// 402): status + body, passed through.
    Ok { status: u16, body: Vec<u8> },
    /// Paid and served. `settlement` is the server's `PAYMENT-RESPONSE`
    /// when present, byte-preserved for audit.
    Paid {
        status: u16,
        body: Vec<u8>,
        settlement: Option<X402Carry<SettlementResponse>>,
    },
    /// Spend policy wants a human — same contract as everywhere else;
    /// the request was NOT retried and nothing was signed or sent.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Spend policy denies (unenabled network, unknown asset, …).
    Denied { policy_reason: String },
    /// The server refused the payment (a second 402 / 400 after
    /// paying): terminal for this attempt; the reservation was
    /// released (per the transport, non-2xx means not settled).
    PaymentRejected { status: u16, message: String },
    /// Transport-level failure.
    Failed { message: String, retryable: bool },
}

/// Response bodies from an external x402 server. Capped so a hostile or
/// compromised endpoint cannot stream until the 30s timeout and exhaust
/// memory — the same discipline the facilitator client and chain checker
/// apply, which this door was missing.
///
/// Larger than the facilitator's 4 MiB cap because this body is the
/// *resource the caller paid for*, not a protocol envelope: a paid API
/// returning a few megabytes of JSON is ordinary. It is still a bound.
const MAX_RESOURCE_BODY: usize = 32 * 1024 * 1024;

/// The outbound paid-HTTP client.
pub struct X402HttpFlow {
    caller: Arc<EntityKeypair>,
    spend: SpendPolicyEngine,
    registry: AssetRegistry,
    signers: std::collections::BTreeMap<String, Arc<dyn SchemeSigner>>,
    clock: Arc<dyn Clock>,
    http: reqwest::Client,
    destinations: crate::http_policy::DestinationPolicy,
    /// Counter making every fetch's local pseudo-quote a distinct one.
    ///
    /// The pseudo-quote's id derives from provider + caller + terms hash +
    /// issued-at. On this door the provider and caller are both this
    /// identity and the terms are whatever the server demanded, so under
    /// a fixed or coarse [`Clock`] two fetches of the same URL derive the
    /// SAME id — and the spend engine, which now treats a repeat
    /// reservation for one quote id as a retry, would count only the
    /// first. Money would leave twice against one budget entry.
    ///
    /// That collapse is wrong here specifically because this door has no
    /// provider-side idempotency: it is documented that one `fetch_paid`
    /// is one attempt. Two attempts are two payments and must be two
    /// reservations.
    ///
    /// **Process-global, not per-flow.** Two `X402HttpFlow` values over
    /// the same spend store would each start at zero, so their first
    /// fetches would collide again under a stopped clock — the same bug
    /// one scope out. Nothing about the counter is per-instance, so it
    /// does not belong to an instance.
    ///
    /// The counter alone is not enough either: it is unique *within* a
    /// process, and a spend policy file is shared across processes. Two
    /// programs run against the same store both start at zero and mint
    /// the same identity, and the second attempt reads as a retry of the
    /// first — the same collapse, one scope further out again. The
    /// counter is therefore qualified by [`process_token`] and the live
    /// pid.
    ///
    /// It is only consulted when there is no approved hold to redeem. A
    /// held quote's identity comes from the spend store, so an approval
    /// stays redeemable no matter how far this counter has moved in the
    /// meantime.
    attempt: &'static std::sync::atomic::AtomicU64,
}

/// See [`X402HttpFlow::attempt`]. One counter per process, because two
/// flows sharing a spend store must not mint the same reservation
/// identity.
static ATTEMPT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A short token distinguishing this process from any other sharing the
/// spend policy file.
///
/// The pid alone would not do it: pids are reused after a process exits,
/// and the exited process's reservations can still be on file. Mixing in
/// the wall clock at first use makes reuse require the same pid *and* the
/// same start nanosecond.
///
/// This is the one place in the crate that reads `SystemTime` rather than
/// a [`Clock`], and deliberately: the injectable clock is the thing being
/// defended against here — a stopped or coarse test clock is exactly what
/// makes two attempts collide. Nothing monetary is decided from this
/// value; it is a local bookkeeping label.
///
/// The live pid is mixed in **at the use site** rather than baked in
/// here, because this is cached in a `OnceLock` and a `fork` carries the
/// cached value — and the attempt counter — into the child. Parent and
/// child would then mint the same identity for two different payments.
/// Reading the pid per attempt costs nothing and cannot be inherited.
fn process_token() -> &'static str {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| {
        let pid = std::process::id();
        let started_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let digest = blake3::hash(format!("{pid}:{started_ns}").as_bytes());
        hex::encode(&digest.as_bytes()[..6])
    })
}

/// Read back a pseudo-quote this door minted and the spend store held.
///
/// Plain deserialization, deliberately: [`PaymentQuote::from_json_bytes`]
/// requires a signature, and there is no signer for these. On this door
/// the provider and the caller are the same identity and there is no
/// provider-issued quote at all — the external server's 402 is the
/// commercial fact, and the pseudo-quote exists only to give the local
/// spend engine something to judge and an operator something to approve.
/// A signature over one's own assertion would prove nothing.
///
/// What carries the weight instead is where the bytes come from: the
/// owner-only spend policy file, keyed by the id the operator approved.
/// The caller re-checks that the decoded quote's own id matches that key,
/// so a store edited underneath us cannot substitute a different quote
/// into an existing approval.
fn decode_local_quote(bytes: &[u8]) -> Option<PaymentQuote> {
    serde_json::from_slice::<PaymentQuote>(bytes).ok()
}

impl X402HttpFlow {
    pub fn new(
        caller: Arc<EntityKeypair>,
        spend: SpendPolicyEngine,
        registry: AssetRegistry,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        Self::with_destination_policy(
            caller,
            spend,
            registry,
            clock,
            // Default: public unicast only.
            //
            // This is the one money-path client whose URL may be chosen
            // by a model rather than an operator, so it gets the
            // strictest policy and local testing opts *in* rather than
            // out. Loopback was admitted here at first, on the reasoning
            // that `is_payment_safe_url` already allows http to it — but
            // that rule is about not putting a bearer authorization on
            // the wire in the clear, which is a different question from
            // what an agent-supplied URL should be allowed to reach.
            // Loopback is where admin surfaces live.
            //
            // A local or self-hosted x402 server is reached by asking for
            // it: `with_destination_policy(PublicOrLoopback)`, or
            // `AllowPrivate` for a LAN node.
            crate::http_policy::DestinationPolicy::PublicOnly,
        )
    }

    /// Build with an explicit destination policy — tighten to
    /// [`PublicOnly`](crate::http_policy::DestinationPolicy::PublicOnly)
    /// when fetch URLs come from a model, or widen to
    /// [`AllowPrivate`](crate::http_policy::DestinationPolicy::AllowPrivate)
    /// for a self-hosted x402 server on an internal network.
    pub fn with_destination_policy(
        caller: Arc<EntityKeypair>,
        spend: SpendPolicyEngine,
        registry: AssetRegistry,
        clock: Arc<dyn Clock>,
        destinations: crate::http_policy::DestinationPolicy,
    ) -> Result<Self, String> {
        // Never follow redirects: both the unpaid probe and the paid
        // retry carry (or are about to carry) a signed EIP-3009
        // authorization — a bearer instrument. Following a 3xx would
        // hand it to an origin we never scoped spend policy against.
        //
        // Pinned TLS roots and the destination policy come from the
        // shared money-path builder (`crate::http_policy`), so the
        // resolver enforcing the policy IS the resolver reqwest dials —
        // no window for DNS to answer differently between check and
        // connect.
        let http = crate::http_policy::client(
            destinations,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(10),
            reqwest::redirect::Policy::none(),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            caller,
            spend,
            registry,
            signers: std::collections::BTreeMap::new(),
            clock,
            http,
            destinations,
            attempt: &ATTEMPT_SEQ,
        })
    }

    /// Register a settlement signer for a CAIP-2 namespace (same seam
    /// as the mesh flow).
    pub fn with_signer(
        mut self,
        namespace: impl Into<String>,
        signer: Arc<dyn SchemeSigner>,
    ) -> Self {
        self.signers.insert(namespace.into(), signer);
        self
    }

    fn can_settle(&self, requirements: &PaymentRequirements) -> bool {
        if requirements.network.starts_with("mock:") {
            return true;
        }
        let namespace = requirements.network.split(':').next().unwrap_or_default();
        requirements.scheme == "exact"
            && (namespace == "eip155" || super::OPAQUE_BLOB_NAMESPACES.contains(&namespace))
            && self.signers.contains_key(namespace)
    }

    /// GET `url`, paying if the server demands it.
    pub async fn fetch_paid(&self, url: &str) -> X402HttpOutcome {
        // -- [0] parse once, up front, and fail closed. Everything that
        //    scopes policy to an origin — the demand-origin re-check below
        //    and the `x402-http/<host>` capability key — reads its host
        //    from THIS parse, never from string surgery on the raw URL.
        //    A URL the client will happily send but we cannot parse is a
        //    denial, not a request scoped to a guessed host.
        let parsed = match reqwest::Url::parse(url) {
            Ok(u) => u,
            Err(e) => {
                return X402HttpOutcome::Denied {
                    policy_reason: format!("refusing to fetch an unparseable URL `{url}`: {e}"),
                }
            }
        };
        let Some(intended_host) = parsed.host_str().map(str::to_owned) else {
            return X402HttpOutcome::Denied {
                policy_reason: format!("refusing to fetch a URL with no host: `{url}`"),
            };
        };
        // Destination policy for an IP-literal host, applied BEFORE the
        // unpaid probe. `GuardedResolver` covers names, but a literal is
        // dialled without a DNS lookup and so never reaches it — and a
        // literal is exactly how `http://169.254.169.254/` is spelled.
        // The probe is the SSRF, so this must gate the probe, not merely
        // the paid retry.
        if let Err(e) = crate::http_policy::check_url_destination(&parsed, self.destinations) {
            return X402HttpOutcome::Denied {
                policy_reason: format!("refusing to fetch `{url}`: {e}"),
            };
        }

        // -- [1] the unpaid attempt. The destination policy is enforced
        //    inside the client's resolver, so it applies to THIS probe —
        //    not merely to the paid retry. The probe is the SSRF: it is
        //    the request an agent-supplied URL can aim at a link-local or
        //    internal address, and it fires before any policy the caller
        //    could otherwise interpose.
        let response = match self.http.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                // A destination refusal happens inside the resolver and
                // reaches here as a connect error, so name the policy —
                // otherwise "error sending request" is all an operator
                // sees when their own configuration refused the address.
                // A destination-policy refusal happens inside the
                // resolver, so it arrives as a connect error — but it is
                // not transient: the policy will refuse the same address
                // forever, and reporting it retryable invites a caller to
                // spin on a denial.
                let refused = crate::http_policy::is_policy_refusal(&e);
                let message = if refused {
                    format!(
                        "{e} (destination policy admits {} — a refused address surfaces here)",
                        self.destinations.describe()
                    )
                } else {
                    e.to_string()
                };
                return X402HttpOutcome::Failed {
                    message,
                    retryable: !refused && (e.is_timeout() || e.is_connect()),
                };
            }
        };
        let status = response.status().as_u16();
        // A redirect is refused, not followed: the client is built with
        // `Policy::none()`, so a 3xx lands here as a real response. Treat
        // it as a hard failure — a moved paid resource must be re-fetched
        // explicitly at its true origin, never chased while a payment is
        // in flight to a host we scoped policy against.
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>");
            return X402HttpOutcome::Failed {
                message: format!(
                    "refusing to follow a {status} redirect to `{location}` on a paid fetch"
                ),
                retryable: false,
            };
        }
        if status != 402 {
            let body = match crate::http_policy::read_bounded(response, MAX_RESOURCE_BODY).await {
                Ok(b) => b,
                Err(e) => {
                    return X402HttpOutcome::Failed {
                        message: format!("reading the response body: {e}"),
                        // A truncated or reset read is worth another go;
                        // only an over-cap body is terminal, because a
                        // peer that sent more than the cap will not send
                        // less next time.
                        retryable: matches!(e, crate::http_policy::ReadError::Transport(_)),
                    };
                }
            };
            return X402HttpOutcome::Ok { status, body };
        }

        // The 402 demand must originate from the host we intend to pay.
        // With redirects disabled this holds by construction, but re-check
        // so the capability key (`x402-http/<host>`) and the signed retry
        // can never be scoped to one origin while the demand — and the
        // pay_to/amount it dictates — was authored by another.
        let demand_host = response.url().host_str().map(str::to_owned);
        if demand_host.as_deref() != Some(intended_host.as_str()) {
            return X402HttpOutcome::Failed {
                message: format!(
                    "402 demand origin `{}` does not match the intended host `{intended_host}`",
                    demand_host.as_deref().unwrap_or("<none>"),
                ),
                retryable: false,
            };
        }

        // The paid retry carries the signed PAYMENT-SIGNATURE (a bearer
        // instrument): refuse to author payment for a cleartext http URL to
        // a remote host. http to loopback stays allowed for local testing.
        if !is_payment_safe_url(url) {
            return X402HttpOutcome::Denied {
                policy_reason: format!(
                    "refusing to send a signed payment over cleartext to `{url}` — use https"
                ),
            };
        }

        // -- [2] the demand, from the PAYMENT-REQUIRED header.
        let Some(required_b64) = response
            .headers()
            .get(HDR_PAYMENT_REQUIRED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
        else {
            return X402HttpOutcome::Failed {
                message: "402 without a PAYMENT-REQUIRED header is not x402 v2".to_string(),
                retryable: false,
            };
        };
        let required: X402Carry<PaymentRequired> = match BASE64
            .decode(required_b64.as_bytes())
            .map_err(|e| e.to_string())
            .and_then(|bytes| X402Carry::from_bytes(bytes).map_err(|e| e.to_string()))
        {
            Ok(c) => c,
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: format!("PAYMENT-REQUIRED header: {e}"),
                    retryable: false,
                }
            }
        };
        let Some(entry) = required.view().accepts.iter().find(|r| self.can_settle(r)) else {
            let offered: Vec<String> = required
                .view()
                .accepts
                .iter()
                .map(|r| format!("({}, {})", r.scheme, r.network))
                .collect();
            return X402HttpOutcome::Denied {
                policy_reason: format!(
                    "no settleable accepts[] entry: the server offers {offered:?}"
                ),
            };
        };

        // -- [3] the local pseudo-quote: the external demand as a
        //    commercial fact the spend engine can judge. Capability key
        //    is per external host, so overrides + approvals scope
        //    sensibly.
        let requirements = match X402Carry::author(entry) {
            Ok(c) => c,
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: false,
                }
            }
        };
        let now_ns = self.clock.now_ns();
        let ttl_ns = entry
            .max_timeout_seconds
            .max(1)
            .saturating_mul(1_000_000_000);
        let capability = format!("x402-http/{intended_host}");

        // Redeem an approval before minting anything new.
        //
        // An approval names a *quote id*. A fetch that came back
        // `RequiresPaymentApproval` handed the operator an id, and the
        // retry has to present that same quote — otherwise every retry
        // mints a fresh unapproved id, holds again, and the approval can
        // never be redeemed however many times the operator grants it.
        //
        // The store is the memo, not a process-local one: the approval
        // lives in the shared spend policy file, so an operator can
        // approve from a different process than the one that will pay.
        // (This mirrors what `CallerPaymentFlow` does on the mesh door.)
        //
        // The held quote is only reusable while it still describes what
        // the server is asking for. Byte-comparing the preserved
        // requirements is exact: a server that changed its price, asset,
        // or payee is making a *different* demand, and an approval for
        // the old one does not carry over to it.
        let mut redeeming_approval: Option<String> = None;
        let held = match self.spend.approved_quote(&capability).await {
            Ok(Some((held_id, held_bytes))) => match decode_local_quote(&held_bytes) {
                Some(held)
                    if held.quote_id == held_id
                        && !held.is_expired_at(now_ns)
                        && held.requirements.bytes() == requirements.bytes() =>
                {
                    redeeming_approval = Some(held_id);
                    Some(held)
                }
                // Expired, unreadable, mis-keyed, or for a demand that has
                // since changed: drop it and fall through to a fresh
                // quote, which will hold again if policy still objects. A
                // new approval for a new quote, never a silent carry-over.
                _ => {
                    let _ = self.spend.clear_approval(&held_id).await;
                    None
                }
            },
            _ => None,
        };

        let quote = match held {
            Some(quote) => quote,
            None => {
                // The attempt counter rides `input_hash`, which feeds
                // `terms_hash` and therefore the quote id — so each
                // payment is its own quote and its own reservation.
                // (`input_hash` is the field for "what this quote is bound
                // to beyond the terms", and on this door the thing it is
                // bound to is this specific attempt.)
                //
                // The live pid rides alongside the cached process token:
                // the token is memoized in a `OnceLock`, so a `fork`
                // carries it — and this counter — into the child, and the
                // two processes would mint one identity for two payments.
                let attempt = self
                    .attempt
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                PaymentQuote::new(
                    self.caller.entity_id().clone(),
                    self.caller.entity_id().clone(),
                    capability.clone(),
                    Some(format!(
                        "x402-http-attempt:{}:{}:{attempt}",
                        process_token(),
                        std::process::id()
                    )),
                    requirements,
                    match self.registry.reference() {
                        Ok(r) => r,
                        Err(e) => {
                            return X402HttpOutcome::Failed {
                                message: e.to_string(),
                                retryable: false,
                            }
                        }
                    },
                    now_ns,
                    now_ns.saturating_add(ttl_ns),
                )
            }
        };

        match self
            .spend
            .check_and_reserve(&quote, &self.registry, now_ns)
            .await
        {
            Ok(SpendDecision::Allowed) => {
                // The approval authorized one payment and this is it.
                // Clearing here rather than after the HTTP round trip is
                // deliberate: a reservation now exists for this quote, so
                // every later attempt on the same id reads as a retry of
                // this payment rather than a new one. Leaving the record
                // behind would make the approval a standing licence to
                // pay this host, which is not what anyone granted.
                if let Some(held_id) = redeeming_approval.take() {
                    let _ = self.spend.clear_approval(&held_id).await;
                }
            }
            Ok(SpendDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            }) => {
                return X402HttpOutcome::RequiresPaymentApproval {
                    quote_id,
                    policy_reason,
                    approve_hint,
                }
            }
            Ok(SpendDecision::Denied { policy_reason }) => {
                return X402HttpOutcome::Denied { policy_reason }
            }
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: false,
                }
            }
        }

        // -- [4] author the payload (same scheme dispatch as the mesh
        //    flow) and retry with PAYMENT-SIGNATURE.
        let payload = match self.author_payload(&quote).await {
            Ok(p) => p,
            Err(message) => {
                self.release(&quote, now_ns).await;
                return X402HttpOutcome::Failed {
                    message,
                    retryable: false,
                };
            }
        };
        let paid_response = match self
            .http
            .get(url)
            .header(HDR_PAYMENT_SIGNATURE, BASE64.encode(payload.bytes()))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // A destination-policy refusal is the one send failure
                // that provably happened BEFORE anything left: it is
                // raised inside the resolver, so no connection was made
                // and no authorization was transmitted. Releasing is
                // therefore correct rather than optimistic — and NOT
                // releasing would let a permanent denial, which a caller
                // may hit repeatedly on the same URL, eat the day's
                // budget a fetch at a time.
                //
                // Every other send failure stays ambiguous: the payment
                // may have landed, so the reservation stands (fail-closed
                // accounting).
                if crate::http_policy::is_policy_refusal(&e) {
                    self.release(&quote, now_ns).await;
                    return X402HttpOutcome::Denied {
                        policy_reason: format!(
                            "destination policy refused the paid retry (admits {}): {e}",
                            self.destinations.describe()
                        ),
                    };
                }
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: e.is_timeout() || e.is_connect(),
                };
            }
        };

        let status = paid_response.status().as_u16();
        let settlement = paid_response
            .headers()
            .get(HDR_PAYMENT_RESPONSE)
            .and_then(|v| v.to_str().ok())
            .and_then(|b64| BASE64.decode(b64.as_bytes()).ok())
            .and_then(|bytes| X402Carry::<SettlementResponse>::from_bytes(bytes).ok());
        // Bounded, like the unpaid probe. The payment has already left at
        // this point, so a body that overruns the cap does not cost the
        // caller their money — but it must not cost them their memory
        // either, and reporting the overrun beats silently truncating.
        let body = match crate::http_policy::read_bounded(paid_response, MAX_RESOURCE_BODY).await {
            Ok(b) => b,
            Err(e) => {
                // The status is already known here, so the reservation
                // decision does not have to wait for the body. A non-2xx
                // means the server refused, and for the chainless mock
                // scheme a refusal is trustworthy enough to release —
                // exactly as it is on the ordinary rejection path below.
                // Without this, a server that refuses and then sends an
                // oversized body consumes the caller's budget until
                // retention sweeps it.
                if !(200..300).contains(&status) && super::reject_releases_reservation(&quote) {
                    self.release(&quote, now_ns).await;
                }
                return X402HttpOutcome::Failed {
                    message: format!("reading the paid response body: {e}"),
                    retryable: false,
                };
            }
        };

        if (200..300).contains(&status) {
            X402HttpOutcome::Paid {
                status,
                body,
                settlement,
            }
        } else {
            // The v2 transport says a non-2xx answer to a paid request
            // means it did not settle — but the server already holds our
            // signed EIP-3009 authorization, a bearer instrument it could
            // submit on-chain regardless of the status it returns. Release
            // the reservation only for the chainless mock scheme; for a
            // real bearer authorization the reservation must stand
            // (fail-closed accounting), mirroring the mesh flow (M1).
            if super::reject_releases_reservation(&quote) {
                self.release(&quote, now_ns).await;
            }
            X402HttpOutcome::PaymentRejected {
                status,
                message: String::from_utf8_lossy(&body[..body.len().min(256)]).into_owned(),
            }
        }
    }

    async fn author_payload(
        &self,
        quote: &PaymentQuote,
    ) -> Result<X402Carry<PaymentPayload>, String> {
        let requirements = quote.requirements.view();
        let payload_object = if requirements.network.starts_with("mock:") {
            serde_json::json!({
                "mock_authorization": hex::encode(self.caller.entity_id().as_bytes()),
                "nonce": quote.quote_id,
            })
        } else if self.can_settle(requirements) && requirements.network.starts_with("eip155:") {
            let signer = self
                .signers
                .get("eip155")
                .ok_or_else(|| "no eip155 signer configured".to_string())?;
            let auth = exact_evm_authorization_for_quote(quote, &signer.address());
            let typed = crate::x402::schemes::exact_evm::typed_data(requirements, &auth)
                .map_err(|e| e.to_string())?;
            let signature = signer
                .sign_typed_data(&typed)
                .await
                .map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_evm::payload_object(&auth, &signature)
        } else if self.can_settle(requirements)
            && super::OPAQUE_BLOB_NAMESPACES
                .contains(&requirements.network.split(':').next().unwrap_or_default())
        {
            // exact / solana | xrpl: the wallet authors the opaque blob
            // from the demanded requirements, via the shared
            // `author_opaque_blob_payload` (identical dispatch to the mesh
            // flow — the two paths cannot drift). Retry honesty on this
            // path: HTTP has no provider-side idempotency (one `fetch_paid`
            // = one attempt), so a re-fetch that re-signs (fresh SPL
            // blockhash / a re-quoted XRPL blob after an expired
            // LastLedgerSequence) is simply the next attempt.
            let namespace = requirements.network.split(':').next().unwrap_or_default();
            let signer = self
                .signers
                .get(namespace)
                .ok_or_else(|| format!("no {namespace} signer configured"))?;
            super::author_opaque_blob_payload(namespace, requirements, signer).await?
        } else {
            return Err(format!(
                "no payload author for scheme `{}` on `{}`",
                requirements.scheme, requirements.network
            ));
        };
        X402Carry::author(&PaymentPayload {
            x402_version: X402_VERSION,
            resource: None,
            accepted: requirements.clone(),
            payload: payload_object,
            extensions: None,
        })
        .map_err(|e| e.to_string())
    }

    async fn release(&self, quote: &PaymentQuote, now_ns: u64) {
        if let Err(e) = self.spend.release_reservation(quote, now_ns).await {
            tracing::warn!(quote_ref = %super::quote_ref(&quote.quote_id), error = %e, "spend reservation release failed");
        }
    }
}

/// Whether a signed payment may be sent to `url`: https anywhere, or http
/// only to a loopback **address literal** (local/self-hosted testing).
/// Anything else would put the PAYMENT-SIGNATURE bearer instrument on the
/// wire in the clear.
///
/// **Delegates to [`crate::http_policy::require_secure_endpoint`]**, which
/// is the same rule the facilitator client and the chain checker apply.
/// It used to be a private copy, and the copy did not get the fix that
/// made the cleartext exception address-level: it granted the exception to
/// the *name* `localhost`, which is whatever DNS says it is. A hosts file
/// or a split-horizon resolver pointing that name at a LAN address turned
/// "http to localhost" into an unencrypted EIP-3009 authorization sent to
/// a remote host — the precise thing this check exists to prevent, on the
/// one door whose URL may be model-chosen.
///
/// Under the default `PublicOnly` the resolver refused that address
/// anyway, so the gap opened only for a caller that had opted into
/// `PublicOrLoopback` or `AllowPrivate`. Narrow — and beside the point.
/// `http_policy` exists because these rules drifted when they were
/// per-client, and one surviving copy is how they drift again.
fn is_payment_safe_url(url: &str) -> bool {
    crate::http_policy::require_secure_endpoint(url).is_ok()
}

// The per-host capability key comes from `Url::host_str()` on the single
// parse at the top of `fetch_paid` — never from splitting the raw URL
// string. A hand-rolled split returns the userinfo/port/case verbatim
// (`api.example.com:443`, `API.example.com`, `user@api.example.com`),
// which misses a configured `x402-http/api.example.com` spend override
// and silently falls back to `defaults` — the wrong direction whenever
// the operator's per-host limit is the tighter one.

#[cfg(test)]
mod tests {
    use super::is_payment_safe_url;

    #[test]
    fn payment_requires_https_except_loopback_literals() {
        assert!(is_payment_safe_url("https://api.example.com/x"));
        assert!(is_payment_safe_url("http://127.0.0.1:8080/x"));
        assert!(is_payment_safe_url("http://[::1]/x"));
        // The v4 rules are not bypassable by spelling the address in v6.
        assert!(is_payment_safe_url("http://[::ffff:127.0.0.1]/x"));
        // Cleartext to a remote host, or a non-web scheme: refused.
        assert!(!is_payment_safe_url("http://api.example.com/x"));
        assert!(!is_payment_safe_url("ftp://api.example.com/x"));
        assert!(!is_payment_safe_url("not-a-url"));
    }

    /// The cleartext exception is address-level, so the NAME `localhost`
    /// does not get it — this door applies the same rule as the
    /// facilitator client and the chain checker.
    ///
    /// A name is whatever DNS says it is. A hosts file or a split-horizon
    /// resolver can point `localhost` at a LAN address, and then this
    /// check would be waving a signed EIP-3009 authorization onto the
    /// wire in the clear, bound for a host that is not this one.
    #[test]
    fn a_signed_payment_is_not_sent_cleartext_to_the_name_localhost() {
        for name in [
            "http://localhost/x",
            "http://LOCALHOST:8080/x",
            "http://localhost.localdomain/x",
        ] {
            assert!(
                !is_payment_safe_url(name),
                "`{name}` is a name, not a loopback literal — it must not get the cleartext \
                 exception"
            );
        }
        // https to the same name is fine: the guard is about cleartext.
        assert!(is_payment_safe_url("https://localhost:8080/x"));
    }
}
