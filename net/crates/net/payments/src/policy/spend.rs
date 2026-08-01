//! The caller-side spend policy engine (Workstream 3).
//!
//! One policy engine, one implementation: the caller's node applies this
//! before anything leaves. The model never decides payment policy — it
//! requests invocation; this engine enforces; approval prompts render in
//! agent UX and the decision lives in the shared locked store.
//!
//! Defaults are doctrine:
//! - **Real networks are config-enabled, never ambient** (the P1
//!   replacement of P0's hard deny): a real network spends only when
//!   explicitly listed in the effective `allowed_networks` — an empty
//!   allowlist enables nothing real, and neither profiles, unsafe
//!   flags, nor approvals bypass network enablement. Default: deny.
//! - **Mock auto-allows only under a dev/test profile or an explicit
//!   unsafe flag** — demos must not train the policy path wrong. In
//!   production profile, every mock spend needs an approval.
//! - Displaying a price never implies authorization to spend it.
//!
//! Auto-allow means: profile admits the network, the allowlists admit the
//! `(network, asset)`, the amount clears `max_per_call`, and the per-day
//! counter clears `max_per_day`. The counter update is a **lock-held
//! read-modify-write** on the shared store — v1-honest: coarse and
//! correct beats clever and racy; two processes hammering the cap can
//! never overspend.
//!
//! Approval mirrors the consent surface verb split: the engine (model-
//! reachable) writes only a *pending* record when it returns the
//! structured `requires_payment_approval`; [`SpendPolicyEngine::approve`]
//! is the operator-only verb, invoked through the SDK consent API by
//! Hermes/OpenClaw-rendered prompts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store::{load_json, mutate_json, mutate_json_if_changed, StoreError};
use crate::core::quote::PaymentQuote;
use crate::core::registry::AssetRegistry;
use crate::core::units::AtomicAmount;

const NS_PER_DAY: u64 = 86_400_000_000_000;
/// Counter housekeeping horizon: entries older than this many days are
/// pruned on write.
const COUNTER_RETAIN_DAYS: u64 = 2;

/// Errors from the spend engine (store I/O / malformed state).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpendError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("spend state malformed: {0}")]
    Malformed(String),
}

/// Per-scope spend limits. All amounts are atomic units of a specific
/// allowed asset; cross-asset budgets are out of scope for P0 (one mock
/// asset exists).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpendLimits {
    /// Require approval for any single call above this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_call: Option<AtomicAmount>,
    /// Require approval once the per-day total would exceed this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_day: Option<AtomicAmount>,
    /// CAIP-2 networks spending is allowed on. For the mock network an
    /// empty list defers to the profile gate; a **real** network spends
    /// only when explicitly listed here — network enablement has no
    /// approval path around it.
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// CAIP-19 asset ids spending is allowed in. Empty defers to the
    /// profile gate for mock assets, same as networks.
    #[serde(default)]
    pub allowed_assets: Vec<String>,
}

/// Runtime posture. There is deliberately no ambient detection — the
/// embedding application states its profile explicitly, and the default
/// everywhere is [`SpendProfile::Production`] (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpendProfile {
    /// Fail-closed: mock spends require approval, real networks deny.
    #[default]
    Production,
    /// Development/test: the mock network auto-allows under limits.
    DevTest,
}

/// Error from [`SpendProfile::parse`] — an unrecognized profile string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSpendProfile(pub String);

impl std::fmt::Display for UnknownSpendProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown payment profile {:?} (expected \"production\" or \"dev_test\")",
            self.0
        )
    }
}

impl std::error::Error for UnknownSpendProfile {}

impl SpendProfile {
    /// Parse the profile string the language bindings accept. Canonical
    /// spellings are `"production"` and `"dev_test"`; `"dev-test"` / `"devtest"`
    /// are accepted aliases. Any other value is a caller error — there is
    /// deliberately **no silent fallback**, so a typo can never quietly widen
    /// the posture to `DevTest` nor quietly narrow an intended `DevTest` to
    /// `Production`.
    ///
    /// The single source of truth for every binding's `payment_profile` /
    /// `paymentProfile` kwarg, so the vocabulary cannot drift per language.
    pub fn parse(s: &str) -> Result<Self, UnknownSpendProfile> {
        match s {
            "production" => Ok(Self::Production),
            "dev_test" | "dev-test" | "devtest" => Ok(Self::DevTest),
            other => Err(UnknownSpendProfile(other.to_string())),
        }
    }
}

impl std::str::FromStr for SpendProfile {
    type Err = UnknownSpendProfile;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// The structured caller-side gate outcome, mirroring the consent shape.
/// The gateway surfaces `RequiresPaymentApproval` as
/// `{status: "requires_payment_approval", quote, policy_reason,
/// approve_hint}` — same contract as `requires_approval`, resolved
/// through the SDK consent API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendDecision {
    /// Policy admits the spend silently (and the per-day counter has
    /// already reserved it).
    Allowed,
    /// Policy wants a human: the quote, why, and how to approve. A
    /// pending approval record now exists in the shared store.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Policy denies outright — no approval path (real networks in P0).
    Denied { policy_reason: String },
}

/// Approval lifecycle in the shared store. The engine writes `Pending`;
/// only the operator verb writes `Approved` — the model must not approve
/// its own future spending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
}

/// One held quote awaiting (or granted) approval. The record carries the
/// quote's canonical bytes so a retry after approval redeems **the same
/// provider-signed quote** the human saw — approval of quote X never
/// authorizes some later quote Y.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub state: ApprovalState,
    /// Capability the quote binds (display form), for redemption lookup.
    #[serde(default)]
    pub capability: String,
    /// The quote envelope's canonical bytes, base64.
    #[serde(default)]
    pub quote_b64: String,
}

/// One outstanding reservation, keyed by the quote that took it.
///
/// The per-day counters are aggregate — `{day}|{network}|{asset}` — so on
/// their own they record *how much* was reserved and nothing about *by
/// whom*. Release then had to be told the amount and the day again, and
/// had no way to tell a first release from a second: two releases for one
/// quote subtracted twice, and an over-large release saturated the whole
/// day's counter to zero, erasing every other reservation for that pair.
///
/// This record is the ownership the counter lacks. Release looks up the
/// exact reservation, decrements what it actually reserved, and removes
/// it — so a second release finds nothing and is a no-op, and the
/// caller's clock stops being part of the correctness argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    /// The day bucket the reservation landed in, so release finds the
    /// same counter regardless of when it runs.
    pub day: u64,
    pub network: String,
    /// The x402 asset locator, as it appears in the counter key.
    pub asset: String,
    /// Atomic units reserved.
    pub amount: AtomicAmount,
}

impl Reservation {
    fn counter_key(&self) -> String {
        format!("{}|{}|{}", self.day, self.network, self.asset)
    }
}

/// On-disk shape. Struct wrapper (not a bare map) for schema headroom,
/// per the store convention.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SpendPolicyFile {
    /// Baseline limits for every capability without an override.
    #[serde(default)]
    pub defaults: SpendLimits,
    /// Per-capability overrides (keyed by `provider/capability` display
    /// form). An override replaces the defaults wholesale — merged-field
    /// semantics invite "which file said 500?" debugging.
    #[serde(default)]
    pub per_capability: BTreeMap<String, SpendLimits>,
    /// Per-day spend counters: `"{day}|{network}|{asset}"` → atomic total.
    #[serde(default)]
    counters: BTreeMap<String, String>,
    /// Approval records keyed by quote id.
    #[serde(default)]
    approvals: BTreeMap<String, ApprovalRecord>,
    /// Outstanding reservations, keyed by quote id — the ownership the
    /// aggregate counters cannot express. Additive: a store written by an
    /// older build simply has none, and its releases stay best-effort.
    #[serde(default)]
    reservations: BTreeMap<String, Reservation>,
}

/// The caller-side spend gate over one shared policy file.
pub struct SpendPolicyEngine {
    path: PathBuf,
    profile: SpendProfile,
    unsafe_mock_auto_allow: bool,
}

impl SpendPolicyEngine {
    pub fn new(path: impl Into<PathBuf>, profile: SpendProfile) -> Self {
        Self {
            path: path.into(),
            profile,
            unsafe_mock_auto_allow: false,
        }
    }

    /// The explicit unsafe flag: auto-allow mock spends even in a
    /// production profile. For demos that cannot run a dev profile; the
    /// name is the warning.
    pub fn with_unsafe_mock_auto_allow(mut self, unsafe_flag: bool) -> Self {
        self.unsafe_mock_auto_allow = unsafe_flag;
        self
    }

    /// Edit policy config (defaults / per-capability overrides) under the
    /// lock. Counters and approvals are engine-managed and not exposed to
    /// the closure.
    pub async fn configure<F>(&self, f: F) -> Result<(), SpendError>
    where
        F: FnOnce(&mut SpendLimits, &mut BTreeMap<String, SpendLimits>),
    {
        mutate_json::<SpendPolicyFile, _, _>(&self.path, |s| {
            f(&mut s.defaults, &mut s.per_capability)
        })
        .await?;
        Ok(())
    }

    /// The caller-side gate: decide, and **reserve** the amount in the
    /// per-day counter when allowed — check and reservation are one
    /// lock-held transaction, which is what makes concurrent callers
    /// unable to overspend.
    pub async fn check_and_reserve(
        &self,
        quote: &PaymentQuote,
        registry: &AssetRegistry,
        now_ns: u64,
    ) -> Result<SpendDecision, SpendError> {
        let requirements = quote.requirements.view();
        let network = requirements.network.clone();
        let amount = AtomicAmount::parse(&requirements.amount)
            .map_err(|e| SpendError::Malformed(e.to_string()))?;
        // Resolve the CAIP-19 identity through the registry the quote was
        // issued under; unregistered assets never reach policy.
        let asset_caip = match registry.check_requirements(requirements) {
            Ok(entry) => entry.id.as_str().to_string(),
            Err(e) => {
                return Ok(SpendDecision::Denied {
                    policy_reason: e.to_string(),
                });
            }
        };
        let is_mock = network.starts_with("mock:");
        let profile_admits_mock =
            matches!(self.profile, SpendProfile::DevTest) || self.unsafe_mock_auto_allow;

        let quote_id = quote.quote_id.clone();
        let capability = quote.capability.clone();
        let day = now_ns / NS_PER_DAY;
        let requirements_asset = requirements.asset.clone();
        let counter_key = format!("{day}|{network}|{requirements_asset}");
        // The quote's canonical bytes are held with a pending approval so a
        // post-approval retry redeems this exact provider-signed quote. They
        // are computed lazily inside `require` (below) — see the note there.
        // A canonicalization failure has no error channel out of the store
        // closure, so it parks here and is re-raised after the transaction.
        let mut canonical_failure: Option<String> = None;

        let approve_hint =
            format!("approve quote {quote_id} via the payments consent API (operator surface)");

        // Conditional-save: only persist when this transaction actually
        // changed durable state. Three independent sources of change, tracked
        // explicitly so a "clean" denial that skips the write can never drop a
        // real mutation:
        //   * housekeeping pruned an expired counter (the trap: a nominal
        //     denial is still dirty if retention removed anything);
        //   * `require` inserted a NEW pending approval (an identical
        //     already-pending record is a no-op → clean);
        //   * a reservation landed in the day counter (always dirty).
        let decision = mutate_json_if_changed::<SpendPolicyFile, _, _>(&self.path, |s| {
            // Housekeeping: drop counters beyond the retention horizon,
            // and the reservations that pointed at them.
            //
            // Reservations are only removed by an explicit release, and a
            // *successful* payment never releases — it has nothing to give
            // back. So without this the map would grow with lifetime
            // payment volume, on a file every payment parses and rewrites
            // under a lock. Once a reservation's counter has aged out it
            // can no longer be released meaningfully, so it is dead.
            //
            // `retain` only ever removes, so a shrink means we pruned.
            let counters_before = s.counters.len();
            s.counters
                .retain(|k, _| counter_day(k).is_some_and(|d| d + COUNTER_RETAIN_DAYS >= day));
            let reservations_before = s.reservations.len();
            s.reservations
                .retain(|_, r| r.day + COUNTER_RETAIN_DAYS >= day);
            let mut dirty =
                s.counters.len() != counters_before || s.reservations.len() != reservations_before;

            let approved = s
                .approvals
                .get(&quote_id)
                .is_some_and(|r| r.state == ApprovalState::Approved);
            let limits = s
                .per_capability
                .get(&capability)
                .unwrap_or(&s.defaults)
                .clone();

            // The model-reachable side writes a pending approval — but only
            // when one is not already recorded. Sets `*dirty` iff it inserts.
            //
            // The quote is canonicalized HERE rather than before the
            // transaction: a full `to_value` + recursive canonical write +
            // base64 over the whole envelope is needed only to hold the exact
            // provider-signed quote beside a NEW pending approval. On the
            // allowed path — and on an already-pending observation, which is
            // the common case under a duplicate storm — it was pure waste.
            //
            // The tradeoff, stated because it is the one direction this is a
            // regression: that work now happens INSIDE the cross-process
            // advisory lock, whereas before it ran ahead of it. Every other
            // mutator of this store waits behind a 1ms-doubling poll loop
            // (see `payments-spend-contention.md`), so the first-approval
            // path lengthens a critical section it used to stay out of. It
            // is taken deliberately: an approval-required decision is
            // operator-gated and rare, one quote's canonical bytes are
            // small, and the paths that are actually hot — allowed, and
            // already-pending — now do none of this work at all. Moving it
            // back out would mean a speculative pre-check to guess whether
            // an approval is coming, which costs a second read on every
            // call to save a rare one.
            //
            // `canonical_bytes` can only fail on a float-bearing or
            // non-object envelope, unreachable for a `PaymentQuote`, but the
            // failure is surfaced rather than swallowed: no approval is
            // recorded (an approval holding no quote bytes is one a
            // post-approval retry cannot redeem), the reason is parked in
            // `failure`, and the caller re-raises it as `SpendError::Malformed`
            // once the transaction ends. Nothing is reserved on that path.
            let require = |s: &mut SpendPolicyFile,
                           dirty: &mut bool,
                           failure: &mut Option<String>,
                           policy_reason: String| {
                if !s.approvals.contains_key(&quote_id) {
                    match crate::core::canonical::canonical_bytes(quote) {
                        Ok(bytes) => {
                            use base64::Engine as _;
                            s.approvals.insert(
                                quote_id.clone(),
                                ApprovalRecord {
                                    state: ApprovalState::Pending,
                                    capability: capability.clone(),
                                    quote_b64: base64::engine::general_purpose::STANDARD
                                        .encode(bytes),
                                },
                            );
                            *dirty = true;
                        }
                        Err(e) => *failure = Some(e.to_string()),
                    }
                }
                SpendDecision::RequiresPaymentApproval {
                    quote_id: quote_id.clone(),
                    policy_reason,
                    approve_hint: approve_hint.clone(),
                }
            };

            let decision: SpendDecision = 'decision: {
                // Real networks are config-enabled, never ambient — the
                // network must be EXPLICITLY listed in the effective limits'
                // allowed_networks (an empty allowlist enables nothing real),
                // and neither profiles, unsafe flags, nor approvals bypass
                // network enablement itself. This replaced P0's hard deny:
                // config, not code — but default remains deny-all.
                if !is_mock && !limits.allowed_networks.contains(&network) {
                    break 'decision SpendDecision::Denied {
                        policy_reason: format!(
                            "real network `{network}` is not enabled for `{capability}` — add it \
                             to allowed_networks (plus a signer + facilitator config) to enable; \
                             the default is deny"
                        ),
                    };
                }

                if !approved {
                    if is_mock {
                        if !profile_admits_mock {
                            break 'decision require(
                                s,
                                &mut dirty,
                                &mut canonical_failure,
                                "mock-network auto-allow requires a dev/test profile or the \
                                 explicit unsafe flag; production profile requires approval per \
                                 spend"
                                    .to_string(),
                            );
                        }
                        if !limits.allowed_networks.is_empty()
                            && !limits.allowed_networks.contains(&network)
                        {
                            break 'decision require(
                                s,
                                &mut dirty,
                                &mut canonical_failure,
                                format!("network `{network}` is not in allowed_networks"),
                            );
                        }
                    }
                    if !limits.allowed_assets.is_empty()
                        && !limits.allowed_assets.contains(&asset_caip)
                    {
                        break 'decision require(
                            s,
                            &mut dirty,
                            &mut canonical_failure,
                            format!("asset `{asset_caip}` is not in allowed_assets"),
                        );
                    }
                    if let Some(cap) = &limits.max_per_call {
                        if amount > *cap {
                            break 'decision require(
                                s,
                                &mut dirty,
                                &mut canonical_failure,
                                format!(
                                    "amount {amount} exceeds max_per_call {cap} for `{capability}`"
                                ),
                            );
                        }
                    }
                }

                // Ownership first: a reservation already on file for this
                // quote means this is a retry, not a second spend.
                //
                // This has to come BEFORE the per-day cap. Checking it
                // after would re-evaluate `max_per_day` against a total
                // that already includes this very reservation, so a retry
                // of the payment that happened to reach the cap would be
                // told it needs approval — for spending it had already
                // been allowed. The record is what lets us tell a retry
                // from a second spend, so it gets consulted first.
                if s.reservations.contains_key(&quote_id) {
                    break 'decision SpendDecision::Allowed;
                }

                // Per-day counter: read, check, reserve — all under this lock.
                let spent = match s.counters.get(&counter_key) {
                    Some(raw) => match AtomicAmount::parse(raw) {
                        Ok(a) => a,
                        Err(e) => {
                            break 'decision SpendDecision::Denied {
                                policy_reason: format!("spend counter corrupt: {e}"),
                            }
                        }
                    },
                    None => AtomicAmount::from_u128(0),
                };
                let new_total = match spent.checked_add(&amount) {
                    Ok(t) => t,
                    Err(_) => {
                        break 'decision SpendDecision::Denied {
                            policy_reason: "per-day counter overflow".to_string(),
                        }
                    }
                };
                if !approved {
                    if let Some(cap) = &limits.max_per_day {
                        if new_total > *cap {
                            break 'decision require(
                                s,
                                &mut dirty,
                                &mut canonical_failure,
                                format!(
                                    "spending {amount} would take today's `{}` total on \
                                     `{network}` to {new_total}, over max_per_day {cap}",
                                    counter_key.split('|').nth(2).unwrap_or("?")
                                ),
                            );
                        }
                    }
                }
                // Approved spend is still spending: it lands in the counter.
                s.counters
                    .insert(counter_key.clone(), new_total.to_canonical_string());
                s.reservations.insert(
                    quote_id.clone(),
                    Reservation {
                        day,
                        network: network.clone(),
                        asset: requirements_asset.clone(),
                        amount: amount.clone(),
                    },
                );
                dirty = true;
                SpendDecision::Allowed
            };
            (decision, dirty)
        })
        .await?;
        // Re-raise a parked canonicalization failure: `require` declined to
        // record an approval it could not attach the quote bytes to, so the
        // caller must see the error rather than a `RequiresPaymentApproval`
        // pointing at a hold that was never taken.
        if let Some(reason) = canonical_failure {
            return Err(SpendError::Malformed(reason));
        }
        Ok(decision)
    }

    /// Release the reservation this quote took, after a terminal failure
    /// where value verifiably did not move (provider rejected pre-settle,
    /// facilitator refused).
    ///
    /// **Idempotent and owner-checked.** The release decrements exactly
    /// what [`Self::check_and_reserve`] recorded for *this quote*, from
    /// the counter it actually landed in, and then forgets the
    /// reservation. A second release for the same quote finds nothing and
    /// does nothing.
    ///
    /// That is a change from the previous behaviour, and the reason is
    /// worth stating: release used to subtract a caller-supplied amount
    /// from a counter derived from the caller's *current* clock, with no
    /// record of what had been reserved. Two releases subtracted twice,
    /// and because the underflow saturated to zero, one over-large
    /// release wiped the entire day's counter for that
    /// `(network, asset)` — freeing budget for every unrelated
    /// reservation in the same bucket and reopening `max_per_day` as a
    /// loss bound.
    ///
    /// `now_ns` no longer selects the counter — the reservation record
    /// carries its own day — and is used only to retire records whose
    /// counters have already aged out.
    pub async fn release_reservation(
        &self,
        quote: &PaymentQuote,
        now_ns: u64,
    ) -> Result<(), SpendError> {
        let quote_id = quote.quote_id.clone();
        let today = now_ns / NS_PER_DAY;
        mutate_json_if_changed::<SpendPolicyFile, _, _>(&self.path, move |s| {
            // Housekeeping: a reservation whose counter has aged past the
            // retention horizon can never be released meaningfully again.
            let before = s.reservations.len();
            s.reservations
                .retain(|_, r| r.day + COUNTER_RETAIN_DAYS >= today);
            let mut dirty = s.reservations.len() != before;

            if let Some(reservation) = s.reservations.remove(&quote_id) {
                dirty = true;
                let key = reservation.counter_key();
                if let Some(raw) = s.counters.get(&key) {
                    if let Ok(current) = AtomicAmount::parse(raw) {
                        // Saturating only as a floor: the reservation
                        // record means the amount is the one that went in,
                        // so an underflow here would be corruption rather
                        // than a mismatched caller.
                        let reduced = current
                            .checked_sub(&reservation.amount)
                            .unwrap_or_else(|_| AtomicAmount::from_u128(0));
                        s.counters.insert(key, reduced.to_canonical_string());
                    }
                }
            }
            ((), dirty)
        })
        .await?;
        Ok(())
    }

    /// The reservation currently on file for `quote_id`, if any.
    /// Diagnostics and tests.
    pub async fn reservation(&self, quote_id: &str) -> Result<Option<Reservation>, SpendError> {
        let state: SpendPolicyFile = load_json(&self.path).await?;
        Ok(state.reservations.get(quote_id).cloned())
    }

    /// Operator-only verb: approve a specific quote. Resolves through the
    /// SDK consent API (Hermes/OpenClaw render the prompt); the shared
    /// store holds the decision. Returns whether state changed.
    ///
    /// **Approves an existing held quote only.** An unknown `quote_id` is
    /// a no-op returning `false`, never a new record: the engine writes
    /// the pending record (with the exact provider-signed quote bytes
    /// attached) when it decides an approval is needed, so an id with no
    /// record is one nothing ever asked about.
    ///
    /// Minting one here would create a record with an empty `capability`
    /// and empty `quote_b64` that [`Self::check_and_reserve`] still reads
    /// as approved — bypassing `max_per_call`, `max_per_day`, and
    /// `allowed_assets` for whatever quote later carried that id, while
    /// [`Self::approved_quote`] skipped it (no bytes to redeem). Reaching
    /// that state requires a later quote to be issued with exactly that
    /// id, and quote ids are content-derived — so it is a quote-id
    /// collision, not something an attacker steers. This is API
    /// integrity rather than a live attack path; "approve" simply should
    /// not be able to invent the thing it approves.
    pub async fn approve(&self, quote_id: &str) -> Result<bool, SpendError> {
        let quote_id = quote_id.to_string();
        let changed = mutate_json_if_changed::<SpendPolicyFile, _, _>(&self.path, move |s| {
            let Some(record) = s.approvals.get_mut(&quote_id) else {
                return (false, false);
            };
            let changed = record.state != ApprovalState::Approved;
            record.state = ApprovalState::Approved;
            (changed, changed)
        })
        .await?;
        Ok(changed)
    }

    /// Operator verb: reject/remove an approval record.
    pub async fn reject(&self, quote_id: &str) -> Result<bool, SpendError> {
        let quote_id = quote_id.to_string();
        let changed = mutate_json::<SpendPolicyFile, _, _>(&self.path, move |s| {
            s.approvals.remove(&quote_id).is_some()
        })
        .await?;
        Ok(changed)
    }

    /// Pending approval requests, for consent UX to render.
    pub async fn pending(&self) -> Result<Vec<String>, SpendError> {
        let state: SpendPolicyFile = load_json(&self.path).await?;
        Ok(state
            .approvals
            .iter()
            .filter(|(_, v)| v.state == ApprovalState::Pending)
            .map(|(k, _)| k.clone())
            .collect())
    }

    /// An approved-but-unredeemed held quote for `capability`, if any:
    /// `(quote_id, canonical quote bytes)`. The flow redeems this before
    /// requesting a fresh quote, so the human's approval applies to the
    /// exact quote they saw.
    pub async fn approved_quote(
        &self,
        capability: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SpendError> {
        use base64::Engine as _;
        let state: SpendPolicyFile = load_json(&self.path).await?;
        for (quote_id, record) in &state.approvals {
            if record.state == ApprovalState::Approved
                && record.capability == capability
                && !record.quote_b64.is_empty()
            {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&record.quote_b64)
                    .map_err(|e| SpendError::Malformed(e.to_string()))?;
                return Ok(Some((quote_id.clone(), bytes)));
            }
        }
        Ok(None)
    }

    /// Drop an approval record (redeemed after a successful pay, or a
    /// stale hold for an expired quote).
    pub async fn clear_approval(&self, quote_id: &str) -> Result<(), SpendError> {
        let quote_id = quote_id.to_string();
        mutate_json::<SpendPolicyFile, _, _>(&self.path, move |s| {
            s.approvals.remove(&quote_id);
        })
        .await?;
        Ok(())
    }

    /// Today's reserved total for a `(network, x402 asset)` pair.
    pub async fn spent_today(
        &self,
        network: &str,
        asset: &str,
        now_ns: u64,
    ) -> Result<AtomicAmount, SpendError> {
        let state: SpendPolicyFile = load_json(&self.path).await?;
        let key = format!("{}|{network}|{asset}", now_ns / NS_PER_DAY);
        match state.counters.get(&key) {
            Some(raw) => AtomicAmount::parse(raw).map_err(|e| SpendError::Malformed(e.to_string())),
            None => Ok(AtomicAmount::from_u128(0)),
        }
    }
}

fn counter_day(key: &str) -> Option<u64> {
    key.split('|').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_fail_closed() {
        let d = SpendLimits::default();
        assert!(d.max_per_call.is_none());
        assert!(d.allowed_networks.is_empty());
    }

    #[test]
    fn counter_keys_parse_their_day() {
        assert_eq!(counter_day("11574|mock:net|musd"), Some(11_574));
        assert_eq!(counter_day("garbage"), None);
    }

    #[test]
    fn spend_profile_parses_canonical_and_aliases_and_rejects_unknown() {
        assert_eq!(
            SpendProfile::parse("production"),
            Ok(SpendProfile::Production)
        );
        for alias in ["dev_test", "dev-test", "devtest"] {
            assert_eq!(SpendProfile::parse(alias), Ok(SpendProfile::DevTest));
        }
        // No silent fallback: an unknown profile is an error, never Production.
        assert!(SpendProfile::parse("yolo").is_err());
        assert!("prod".parse::<SpendProfile>().is_err());
        // The error names the offending value.
        assert!(SpendProfile::parse("nope")
            .unwrap_err()
            .to_string()
            .contains("nope"));
    }
}
