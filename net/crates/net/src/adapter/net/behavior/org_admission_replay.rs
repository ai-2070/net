//! OA-2 §2.5 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the
//! admission replay guard.
//!
//! Authentication is never replay prevention (a pinned invariant):
//! a valid [`OrgCallProof`](super::org_call::OrgCallProof) can be
//! captured off the wire and re-sent byte-for-byte until it
//! expires. The §2.4 admission order therefore ends every accepted
//! proof with an ATOMIC insert-or-deny into this guard, keyed on
//! the nRPC correlation identity `(caller, call_id)` — NOT request
//! content — BEFORE the handler runs.
//!
//! ```text
//! same (caller, call_id), same binding digest      → Replay
//! same (caller, call_id), different binding digest  → CallIdCollision
//! new  (caller, call_id)                            → Admitted (recorded)
//! ```
//!
//! Keying on `(caller, call_id, binding_digest)` would be WRONG:
//! the same caller could reuse a `call_id` with a freshly signed
//! DIFFERENT binding and mint a new map key, side-stepping the
//! guard. Under correlation-identity keying, ANY reuse of
//! `(caller, call_id)` before expiry denies without a second
//! handler invocation — and the two reuse shapes are
//! distinguishable so a caller bug (id collision) reads
//! differently from an attack (replay).
//!
//! # Retention and capacity
//!
//! Entries are retained to the proof's expiry on a MONOTONIC clock
//! (a wall-clock jump must not evict a still-live guard). An
//! UNEXPIRED entry is NEVER evicted — the guard would otherwise
//! forget a proof still inside its replay window. A bounded map
//! ([`AdmissionReplayConfig::max_entries`]) caps memory against a
//! caller flooding novel `call_id`s; once full of unexpired
//! entries, new admissions DENY with [`ReplayOutcome::CapacityExhausted`]
//! and bump a metric rather than evicting a live guard. The guard
//! is VOLATILE by contract — cross-restart idempotency is the
//! application's concern (as it is for nRPC today).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use super::org::OrgId;
use crate::adapter::net::identity::EntityId;

/// Provisional ceiling on tracked in-flight+recent admissions
/// (plan §2.5: "constants frozen after measurement"). Sized so a
/// burst of legitimate concurrent callers fits comfortably while a
/// single caller cannot exhaust process memory with novel
/// `call_id`s. Flagged for OA-2 measurement before freeze.
pub const DEFAULT_MAX_REPLAY_ENTRIES: usize = 65_536;

/// Provisional per-caller ceiling (E1.5, verdict §10). Policy runs
/// AFTER replay insertion, so even a policy-vetoed VALID proof
/// consumes a slot; without a per-caller sub-ceiling a single
/// credentialed caller could fill the whole global map and starve
/// every other org fail-closed. Sized to admit a healthy concurrent
/// burst from one caller while leaving ample global headroom for
/// everyone else (16× fits under the global default). Flagged for
/// OA-2 measurement before freeze.
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER: usize = 4_096;

/// Entries reserved for the PROVIDER'S OWN owner org (§5).
///
/// The per-caller ceiling is correct in isolation and does not COMPOSE: with
/// `max_entries / max_entries_per_caller == 16`, sixteen identities saturate
/// the global map, after which the provider denies its OWN owner-org callers
/// fail-closed with a retryable `Unavailable`. Minting sixteen identities is
/// one org-admin action for a single grantee org, so the coalition is trivial
/// to assemble.
///
/// Raising `max_entries` does not fix that — it only changes the coalition
/// size. Partitioning does: external callers can never touch this reserve, so
/// no external coalition of any size can deny the provider's own org.
pub const DEFAULT_OWNER_RESERVED_REPLAY_ENTRIES: usize = 16_384;

/// Aggregate ceiling for ONE external acting organization, across ALL of its
/// member identities (§5).
///
/// This is the quota that actually defeats the coalition: sixteen identities
/// from one grantee org share ONE allocation rather than getting sixteen. It
/// is keyed on the VERIFIED acting organization — never the certificate
/// issuer, the peer/session, or any claimed wire field — because those are
/// either attacker-chosen or free to mint.
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_EXTERNAL_ORG: usize = 4_096;

/// Replay-guard ceilings — a global map cap plus a per-caller
/// sub-ceiling (E1.5) so one caller cannot consume another's
/// allocation.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionReplayConfig {
    /// Maximum simultaneously-retained `(caller, call_id)`
    /// entries across ALL callers. At capacity, a novel admission
    /// denies rather than evicting an unexpired guard.
    pub max_entries: usize,
    /// Maximum simultaneously-retained entries for ONE caller.
    /// Checked before the global cap, so a flooding caller hits its
    /// own ceiling first and never denies other callers.
    pub max_entries_per_caller: usize,
    /// Entries within [`Self::max_entries`] reserved for the provider's OWN
    /// owner org, which external callers can never consume (§5).
    ///
    /// External traffic is therefore bounded by
    /// `max_entries - owner_reserved_entries`, and no external coalition — of
    /// any size, from any number of orgs — can deny an owner-org call.
    pub owner_reserved_entries: usize,
    /// Aggregate ceiling for ONE external acting org across all of its member
    /// identities (§5). The provider's own owner org is deliberately NOT
    /// subject to this: it is bounded per-identity and by the global cap, and
    /// may borrow whatever external capacity is idle.
    pub max_entries_per_external_org: usize,
}

impl Default for AdmissionReplayConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_REPLAY_ENTRIES,
            max_entries_per_caller: DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER,
            owner_reserved_entries: DEFAULT_OWNER_RESERVED_REPLAY_ENTRIES,
            max_entries_per_external_org: DEFAULT_MAX_REPLAY_ENTRIES_PER_EXTERNAL_ORG,
        }
    }
}

impl AdmissionReplayConfig {
    /// Enforce the ceiling invariant (Kyra E1 audit): both bounds are
    /// positive AND the per-caller ceiling is STRICTLY below the
    /// global one. A `max_entries_per_caller >= max_entries` would let
    /// a single caller fill the entire global guard and starve every
    /// other org — the exact starvation the per-caller ceiling exists
    /// to prevent. Validated loudly at construction rather than
    /// silently clamped.
    pub fn validate(&self) -> Result<(), ReplayConfigError> {
        if self.max_entries == 0 {
            return Err(ReplayConfigError::ZeroGlobalCeiling);
        }
        if self.max_entries_per_caller == 0 {
            return Err(ReplayConfigError::ZeroPerCallerCeiling);
        }
        if self.max_entries_per_caller >= self.max_entries {
            return Err(ReplayConfigError::PerCallerNotBelowGlobal {
                per_caller: self.max_entries_per_caller,
                global: self.max_entries,
            });
        }
        if self.max_entries_per_external_org == 0 {
            return Err(ReplayConfigError::ZeroPerExternalOrgCeiling);
        }
        // A reserve at or above the global cap would leave external callers
        // no capacity at all — protected cross-org RPC would be dead on
        // arrival rather than merely bounded.
        if self.owner_reserved_entries >= self.max_entries {
            return Err(ReplayConfigError::OwnerReserveNotBelowGlobal {
                reserved: self.owner_reserved_entries,
                global: self.max_entries,
            });
        }
        // The per-org quota must fit inside the external pool, or the pool
        // bound would be unreachable and the org quota decorative.
        let external_pool = self.max_entries - self.owner_reserved_entries;
        if self.max_entries_per_external_org > external_pool {
            return Err(ReplayConfigError::PerExternalOrgAboveExternalPool {
                per_org: self.max_entries_per_external_org,
                external_pool,
            });
        }
        Ok(())
    }
}

/// An invalid [`AdmissionReplayConfig`] (Kyra E1 audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayConfigError {
    /// `max_entries == 0` — the global guard could never admit.
    #[error("replay max_entries must be > 0")]
    ZeroGlobalCeiling,
    /// `max_entries_per_caller == 0` — no caller could ever admit.
    #[error("replay max_entries_per_caller must be > 0")]
    ZeroPerCallerCeiling,
    /// `max_entries_per_caller >= max_entries` — one caller could
    /// consume the entire global guard.
    #[error("replay max_entries_per_caller ({per_caller}) must be < max_entries ({global})")]
    PerCallerNotBelowGlobal {
        /// The configured per-caller ceiling.
        per_caller: usize,
        /// The configured global ceiling.
        global: usize,
    },
    /// `max_entries_per_external_org == 0` — no external org could admit.
    #[error("replay max_entries_per_external_org must be > 0")]
    ZeroPerExternalOrgCeiling,
    /// `owner_reserved_entries >= max_entries` — external callers would have
    /// no capacity at all.
    #[error(
        "replay owner_reserved_entries ({reserved}) must be < max_entries ({global}); \
         a reserve at or above the global cap leaves external callers nothing"
    )]
    OwnerReserveNotBelowGlobal {
        /// The configured owner reserve.
        reserved: usize,
        /// The configured global ceiling.
        global: usize,
    },
    /// The per-external-org quota cannot fit inside the external pool.
    #[error(
        "replay max_entries_per_external_org ({per_org}) must be <= the external pool \
         ({external_pool} = max_entries - owner_reserved_entries)"
    )]
    PerExternalOrgAboveExternalPool {
        /// The configured per-external-org ceiling.
        per_org: usize,
        /// The derived external pool size.
        external_pool: usize,
    },
}

/// The outcome of an admission check. Only [`Self::Admitted`] lets
/// the handler run; the §2.4 engine maps the others to typed
/// `AdmissionDenied` reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// First sight of this `(caller, call_id)` within its window —
    /// recorded; the handler may run.
    Admitted,
    /// The SAME proof (identical binding digest) re-presented
    /// before expiry — a replay.
    Replay,
    /// The same `(caller, call_id)` with a DIFFERENT binding
    /// digest — a correlation-id collision (caller bug or a
    /// forged reuse of an id).
    CallIdCollision,
    /// The GLOBAL guard is full of still-live entries; admitting
    /// would require evicting an unexpired guard, so this call is
    /// denied fail-closed.
    CapacityExhausted,
    /// THIS caller already holds the maximum simultaneously-retained
    /// entries (E1.5). Denies only this caller — every other
    /// caller's allocation is untouched, so one flooding org cannot
    /// starve the rest.
    PerCallerCapacityExhausted,
    /// THIS external acting ORGANIZATION has consumed its aggregate
    /// allocation, across all of its member identities (§5).
    ///
    /// Distinct from [`Self::PerCallerCapacityExhausted`] and from
    /// [`Self::CapacityExhausted`] on purpose: it is the signal that one
    /// grantee org is behaving abusively, which neither of the others can
    /// express. Per-caller exhaustion names a single identity — and the whole
    /// point of the attack is that identities are free to mint — while global
    /// exhaustion suggests fleet-wide pressure the operator cannot attribute.
    PerOrganizationCapacityExhausted,
    /// The EXTERNAL pool (`max_entries - owner_reserved_entries`) is full of
    /// live entries, with no single org over its own quota (§5).
    ///
    /// Beyond the requested per-org outcome, because conflating it with either
    /// neighbour would mislead: it is not one abusive grantee, and it is NOT
    /// global exhaustion — the owner reserve is by construction still free, so
    /// the provider's own org is unaffected and no operator action against a
    /// particular org is indicated. Reaching it means genuinely many distinct
    /// external orgs are active at once.
    ExternalPoolCapacityExhausted,
}

/// The VERIFIED principal an admission is charged to (§5).
///
/// A struct rather than loose arguments because WHICH identity each quota is
/// keyed on is the whole security property, and a positional `&OrgId, &OrgId`
/// pair would be trivial to transpose at a call site.
#[derive(Debug, Clone, Copy)]
pub struct ReplayPrincipal<'a> {
    /// The caller entity, resolved from the AUTHENTICATED direct session —
    /// never a request-body field.
    pub caller: &'a EntityId,
    /// The org the caller is VERIFIED to be acting for: taken from the
    /// org-signed membership certificate and cross-checked against the
    /// dispatcher grant by `verify_org_admission`.
    ///
    /// Deliberately NOT the certificate ISSUER, the peer/session, or any
    /// claimed wire field. The issuer is attacker-chosen for a self-minted
    /// org; sessions and identities are free to mint, which is exactly what
    /// makes the sixteen-identity coalition cheap. The acting org is the
    /// coarsest thing an attacker cannot fabricate without the provider
    /// already trusting it.
    pub acting_org: &'a OrgId,
    /// The PROVIDER's own owner org — the beneficiary of the reserve.
    pub provider_owner_org: &'a OrgId,
}

impl ReplayPrincipal<'_> {
    /// Whether this admission is charged to the provider's own org, and so
    /// draws on the reserve rather than the external pool.
    fn is_owner_org(&self) -> bool {
        self.acting_org == self.provider_owner_org
    }
}

struct ReplayEntry {
    binding_digest: [u8; 32],
    /// Monotonic instant at/after which this entry is reusable.
    expires_at: Instant,
    /// The acting org this entry is charged to (§5).
    ///
    /// Carried on the ENTRY so reclamation can decrement the org counter and
    /// the external-pool counter without re-deriving anything: an expired
    /// entry must return its slot to exactly the quotas it consumed, and by
    /// the time it expires the request that created it is long gone.
    acting_org: OrgId,
    /// Whether this entry drew on the external pool (i.e. was NOT owner-org).
    ///
    /// Cached rather than recomputed at reclaim time because the provider's
    /// owner org can CHANGE under a re-adopt; recomputing would then return a
    /// slot to the wrong pool and permanently skew the accounting.
    external: bool,
}

/// The mutex-guarded state. Nested `caller → (call_id → entry)` so
/// the per-caller ceiling and per-caller reclamation touch ONLY one
/// caller's entries (E1.5); `total` mirrors the summed inner lengths
/// so the global cap is a field read, not an O(callers) sum.
#[derive(Default)]
struct ReplayState {
    by_caller: HashMap<EntityId, HashMap<u64, ReplayEntry>>,
    total: usize,
    /// Live entries per VERIFIED acting org (§5). The aggregate quota that
    /// makes a coalition of identities share ONE allocation.
    by_org: HashMap<OrgId, usize>,
    /// Live entries drawing on the EXTERNAL pool — i.e. every entry whose
    /// acting org is not the provider's own. Mirrors the summed external
    /// `by_org` values so the pool bound is a field read.
    external_total: usize,
}

impl ReplayState {
    /// Charge one entry to every counter it consumes. The four counters move
    /// together, under the caller's lock, or not at all.
    fn charge(&mut self, entry_external: bool, acting_org: &OrgId) {
        self.total += 1;
        *self.by_org.entry(*acting_org).or_insert(0) += 1;
        if entry_external {
            self.external_total += 1;
        }
    }

    /// Release one reclaimed entry from every counter it consumed.
    ///
    /// The mirror of [`Self::charge`]. `total`, the per-org count and the
    /// external pool must be decremented in step, or a quota drifts upward
    /// forever and eventually denies a legitimate caller with no live entries
    /// to justify it — a leak that only manifests under sustained load, which
    /// is when it is hardest to diagnose.
    fn release(&mut self, entry: &ReplayEntry) {
        self.total -= 1;
        if let Some(count) = self.by_org.get_mut(&entry.acting_org) {
            *count -= 1;
            if *count == 0 {
                self.by_org.remove(&entry.acting_org);
            }
        }
        if entry.external {
            self.external_total -= 1;
        }
    }

    /// Drop `caller`'s expired entries (and the caller bucket if it
    /// empties), releasing each from every counter it held.
    fn reclaim_caller(&mut self, caller: &EntityId, now: Instant) {
        let Some(inner) = self.by_caller.get_mut(caller) else {
            return;
        };
        let expired: Vec<ReplayEntry> = {
            let mut drained = Vec::new();
            inner.retain(|_, e| {
                if e.expires_at > now {
                    true
                } else {
                    drained.push(ReplayEntry {
                        binding_digest: e.binding_digest,
                        expires_at: e.expires_at,
                        acting_org: e.acting_org,
                        external: e.external,
                    });
                    false
                }
            });
            drained
        };
        let empty = inner.is_empty();
        for entry in &expired {
            self.release(entry);
        }
        if empty {
            self.by_caller.remove(caller);
        }
    }

    /// Drop every expired entry across all callers, releasing each from every
    /// counter it held. Returns the number reclaimed.
    fn reclaim_all(&mut self, now: Instant) -> usize {
        let mut released: Vec<ReplayEntry> = Vec::new();
        self.by_caller.retain(|_, inner| {
            inner.retain(|_, e| {
                if e.expires_at > now {
                    true
                } else {
                    released.push(ReplayEntry {
                        binding_digest: e.binding_digest,
                        expires_at: e.expires_at,
                        acting_org: e.acting_org,
                        external: e.external,
                    });
                    false
                }
            });
            !inner.is_empty()
        });
        for entry in &released {
            self.release(entry);
        }
        released.len()
    }

    /// Live entries charged to `org`.
    fn org_live(&self, org: &OrgId) -> usize {
        self.by_org.get(org).copied().unwrap_or(0)
    }
}

/// Burst of FAILED admissions one authenticated peer may cost before it is
/// throttled (§6). See [`AdmissionFailureLimiter`].
pub const DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER: u32 = 64;

/// Failed-admission budget refilled per second, per peer (§6).
pub const DEFAULT_FAILED_ADMISSION_REFILL_PER_SEC: u32 = 8;

/// Maximum peers tracked at once; the oldest idle entry is reclaimed at
/// capacity (§6). Bounded so the limiter cannot itself become the memory
/// exhaustion it exists to prevent.
pub const DEFAULT_MAX_RATE_LIMITED_PEERS: usize = 4_096;

/// Per-peer throttle on the SIGNATURE work a failing caller can compel (§6).
///
/// # The asymmetry this closes
///
/// A peer needs only a TOFU-pinned session and NO org credentials to reach the
/// expensive part of the gate. It self-mints an `OrgKeypair`, issues ITSELF a
/// genuinely valid membership certificate and dispatcher grant under that key,
/// and attaches a garbage capability grant naming the provider's (public)
/// owner org. Every cheap plaintext check passes — none of them verifies a
/// signature — and the gate then performs THREE `ed25519 verify_strict`
/// operations before denying `CapabilityGrantInvalid`.
///
/// Nothing bounded that. Failed admissions deliberately consume no replay
/// slot (the guard records only ADMITTED calls, so a denial cannot evict a
/// legitimate entry), so the replay ceilings — including the §5 trust-domain
/// partition — never see this traffic at all.
///
/// # Why it charges on FAILURE rather than per attempt
///
/// A per-attempt limiter would throttle honest callers, and picking a rate
/// that suits every deployment is exactly the guess that makes such limits
/// wrong. But the attacker's distinguishing property is not its RATE — it is
/// that its admissions always FAIL. A legitimate caller's succeed.
///
/// So a successful admission costs nothing at all: an honest peer can drive
/// the gate as fast as it likes and never touch this. Only failures draw on
/// the bucket, and when it empties the peer is refused BEFORE the signature
/// work rather than after — which is the whole point, since the work is the
/// resource being protected.
///
/// A legitimately misconfigured client (expired proof, wrong capability) fails
/// at a low rate and the refill absorbs it; a client failing faster than the
/// refill is, by construction, either broken or hostile, and throttling it is
/// correct in both cases.
///
/// # These are SAFE defaults, not universal limits
///
/// 64 burst with 8/s refill bounds one peer to ~1.2 ms/s of verification CPU
/// while letting a reconnect storm through untouched. An operator running
/// protected RPC where clients legitimately fail admission in bursts can raise
/// the envelope knowingly via [`MeshNodeConfig`]; the point is that the
/// failure mode is bounded and attributable, not that the numbers suit every
/// workload.
///
/// [`MeshNodeConfig`]: crate::adapter::net::MeshNodeConfig
pub struct AdmissionFailureLimiter {
    buckets: Mutex<HashMap<u64, PeerBucket>>,
    config: AdmissionRateLimitConfig,
    /// Admissions refused because the peer's failure budget was exhausted.
    throttled: AtomicU64,
}

/// Tunables for [`AdmissionFailureLimiter`] (§6).
#[derive(Debug, Clone, Copy)]
pub struct AdmissionRateLimitConfig {
    /// Burst of failed admissions one peer may cost before throttling.
    pub max_failed_per_peer: u32,
    /// Budget refilled per second, per peer.
    pub refill_per_sec: u32,
    /// Maximum peers tracked simultaneously.
    pub max_tracked_peers: usize,
}

impl Default for AdmissionRateLimitConfig {
    fn default() -> Self {
        Self {
            max_failed_per_peer: DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER,
            refill_per_sec: DEFAULT_FAILED_ADMISSION_REFILL_PER_SEC,
            max_tracked_peers: DEFAULT_MAX_RATE_LIMITED_PEERS,
        }
    }
}

impl AdmissionRateLimitConfig {
    /// Reject a degenerate envelope loudly rather than clamping it.
    pub fn validate(&self) -> Result<(), ReplayConfigError> {
        if self.max_failed_per_peer == 0 {
            return Err(ReplayConfigError::ZeroPerCallerCeiling);
        }
        if self.refill_per_sec == 0 {
            // A zero refill makes the first burst permanent: a peer that
            // exhausts its budget is throttled forever, so a single expired
            // proof at startup would take a client out until restart.
            return Err(ReplayConfigError::ZeroPerCallerCeiling);
        }
        if self.max_tracked_peers == 0 {
            return Err(ReplayConfigError::ZeroGlobalCeiling);
        }
        Ok(())
    }
}

struct PeerBucket {
    /// Remaining failed-admission allowance.
    tokens: u32,
    /// When `tokens` was last refilled.
    last_refill: Instant,
    /// Last touch, for idle reclamation at capacity.
    last_seen: Instant,
}

impl AdmissionFailureLimiter {
    /// A limiter with the given envelope, VALIDATED.
    pub fn try_new(config: AdmissionRateLimitConfig) -> Result<Self, ReplayConfigError> {
        config.validate()?;
        Ok(Self {
            buckets: Mutex::new(HashMap::new()),
            config,
            throttled: AtomicU64::new(0),
        })
    }

    /// A limiter with the default envelope (always valid).
    pub fn with_defaults() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            config: AdmissionRateLimitConfig::default(),
            throttled: AtomicU64::new(0),
        }
    }

    /// May `from_node` attempt an admission right now?
    ///
    /// Call BEFORE the signature work. `false` means the peer has spent its
    /// failure budget and must be denied cheaply.
    pub fn may_attempt(&self, from_node: u64, now: Instant) -> bool {
        let mut buckets = self.buckets.lock();
        let cfg = self.config;
        match buckets.get_mut(&from_node) {
            None => true, // unseen peer: no failures charged yet
            Some(bucket) => {
                Self::refill(bucket, cfg, now);
                bucket.last_seen = now;
                if bucket.tokens > 0 {
                    true
                } else {
                    self.throttled.fetch_add(1, Ordering::Relaxed);
                    false
                }
            }
        }
    }

    /// Charge one failed admission to `from_node`.
    ///
    /// Called ONLY on a denial. A successful admission costs nothing, which is
    /// what keeps honest traffic entirely unaffected.
    pub fn on_failure(&self, from_node: u64, now: Instant) {
        let mut buckets = self.buckets.lock();
        let cfg = self.config;
        if !buckets.contains_key(&from_node) {
            if buckets.len() >= cfg.max_tracked_peers {
                // Reclaim the least-recently-seen peer. Safe to evict: losing a
                // bucket only restores that peer's full allowance, and the
                // evicted one is by definition the least active.
                if let Some(oldest) = buckets
                    .iter()
                    .min_by_key(|(_, b)| b.last_seen)
                    .map(|(peer, _)| *peer)
                {
                    buckets.remove(&oldest);
                }
            }
            buckets.insert(
                from_node,
                PeerBucket {
                    tokens: cfg.max_failed_per_peer,
                    last_refill: now,
                    last_seen: now,
                },
            );
        }
        if let Some(bucket) = buckets.get_mut(&from_node) {
            Self::refill(bucket, cfg, now);
            bucket.tokens = bucket.tokens.saturating_sub(1);
            bucket.last_seen = now;
        }
    }

    fn refill(bucket: &mut PeerBucket, cfg: AdmissionRateLimitConfig, now: Instant) {
        let elapsed = now.saturating_duration_since(bucket.last_refill);
        let secs = elapsed.as_secs();
        if secs == 0 {
            return;
        }
        let gained = secs.saturating_mul(u64::from(cfg.refill_per_sec));
        let gained = u32::try_from(gained).unwrap_or(u32::MAX);
        bucket.tokens = bucket
            .tokens
            .saturating_add(gained)
            .min(cfg.max_failed_per_peer);
        bucket.last_refill = now;
    }

    /// Admissions refused because the peer's failure budget was exhausted.
    pub fn throttled_denials(&self) -> u64 {
        self.throttled.load(Ordering::Relaxed)
    }

    /// Remaining failure allowance for `from_node` (test/metric surface).
    pub fn tokens_for(&self, from_node: u64) -> u32 {
        self.buckets
            .lock()
            .get(&from_node)
            .map_or(self.config.max_failed_per_peer, |b| b.tokens)
    }

    /// Peers currently tracked (test/metric surface).
    pub fn tracked_peers(&self) -> usize {
        self.buckets.lock().len()
    }
}
/// The volatile admission replay guard. One per provider node.
pub struct AdmissionReplayGuard {
    entries: Mutex<ReplayState>,
    config: AdmissionReplayConfig,
    /// Count of admissions denied for GLOBAL capacity — a metric
    /// surface (§2.5: "deny + metric on exhaustion").
    capacity_denials: AtomicU64,
    /// Count of admissions denied for PER-CALLER capacity (E1.5) —
    /// a separate metric so operators can tell a fleet-wide flood
    /// from a single abusive caller.
    per_caller_denials: AtomicU64,
    /// Count of admissions denied because ONE EXTERNAL ORG exhausted its
    /// aggregate allocation (§5) — the signal that identifies an abusive
    /// grantee, which neither the global nor the per-caller counter can.
    per_org_denials: AtomicU64,
    /// Count of admissions denied because the EXTERNAL POOL was full with no
    /// single org over quota — genuinely many active external orgs, and
    /// notably NOT a state in which the provider's own org is affected.
    external_pool_denials: AtomicU64,
}

impl AdmissionReplayGuard {
    /// A guard with the given ceilings, VALIDATED (Kyra E1 audit) —
    /// see [`AdmissionReplayConfig::validate`]. Prefer this over
    /// [`Self::new`] on any config not known-good at compile time.
    pub fn try_new(config: AdmissionReplayConfig) -> Result<Self, ReplayConfigError> {
        config.validate()?;
        Ok(Self::from_validated(config))
    }

    fn from_validated(config: AdmissionReplayConfig) -> Self {
        Self {
            entries: Mutex::new(ReplayState::default()),
            config,
            capacity_denials: AtomicU64::new(0),
            per_caller_denials: AtomicU64::new(0),
            per_org_denials: AtomicU64::new(0),
            external_pool_denials: AtomicU64::new(0),
        }
    }

    /// A guard with the given ceilings. Panics on an invalid config
    /// (loud, not silently clamped) — use [`Self::try_new`] when the
    /// config comes from untrusted/dynamic input.
    pub fn new(config: AdmissionReplayConfig) -> Self {
        match Self::try_new(config) {
            Ok(guard) => guard,
            Err(e) => panic!("invalid AdmissionReplayConfig: {e}"),
        }
    }

    /// A guard with the default ceilings (always valid).
    pub fn with_defaults() -> Self {
        Self::from_validated(AdmissionReplayConfig::default())
    }

    /// Atomic insert-or-deny (the last step of §2.4). `now` is the
    /// monotonic clock (real callers pass `Instant::now()`;
    /// `expires_at` is `now + proof-remaining-ttl + skew`,
    /// precomputed by the caller so the guard never touches a wall
    /// clock).
    ///
    /// One lock acquisition covers eviction, the collision check,
    /// and the insert, so two concurrent presentations of one
    /// proof can never both see "absent" and both admit.
    pub fn admit(
        &self,
        principal: ReplayPrincipal<'_>,
        call_id: u64,
        binding_digest: [u8; 32],
        expires_at: Instant,
        now: Instant,
    ) -> ReplayOutcome {
        let caller = principal.caller;
        let external = !principal.is_owner_org();
        let mut st = self.entries.lock();

        // An existing entry for this exact `(caller, call_id)`:
        // replay vs collision, UNLESS it has expired (then it is
        // reusable — the window closed, so this is a legitimate new
        // call reusing the id). Handled under one `get_mut` so the
        // expired overwrite touches neither `total` nor the
        // per-caller count (the key stays occupied).
        if let Some(inner) = st.by_caller.get_mut(caller) {
            if let Some(existing) = inner.get(&call_id) {
                if existing.expires_at > now {
                    return if existing.binding_digest == binding_digest {
                        ReplayOutcome::Replay
                    } else {
                        ReplayOutcome::CallIdCollision
                    };
                }
                // Expired overwrite REUSES the occupied key, so no counter
                // moves — the entry it replaces was already charged and is
                // charged to the same principal by construction (the key is
                // `(caller, call_id)` and the caller cannot change orgs
                // mid-window without a new membership certificate).
                inner.insert(
                    call_id,
                    ReplayEntry {
                        binding_digest,
                        expires_at,
                        acting_org: *principal.acting_org,
                        external,
                    },
                );
                return ReplayOutcome::Admitted;
            }
        }

        // New key for this caller. Per-caller ceiling FIRST (E1.5) so
        // a flooding caller hits its own limit before it can pressure
        // the global cap. At capacity, reclaim only THIS caller's
        // expired slots; if still full, deny only this caller.
        let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
        if caller_live >= self.config.max_entries_per_caller {
            st.reclaim_caller(caller, now);
            let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
            if caller_live >= self.config.max_entries_per_caller {
                self.per_caller_denials.fetch_add(1, Ordering::Relaxed);
                return ReplayOutcome::PerCallerCapacityExhausted;
            }
        }

        // §5 — the TRUST-DOMAIN quotas, checked before the global cap so a
        // flooding org exhausts its own allocation first and the owner
        // reserve is never reachable from outside.
        //
        // Both are keyed on the VERIFIED acting org. Sixteen identities from
        // one grantee org therefore share ONE allocation instead of getting
        // sixteen, which is precisely what made the coalition cheap: minting
        // identities is a single org-admin action, minting a trusted ORG is
        // not.
        //
        // Owner-org traffic is deliberately exempt from both: it is bounded
        // per-identity and by the global cap, and may borrow whatever external
        // capacity is idle. It cannot starve external callers, because the
        // external pool is a floor for them in exactly the way the reserve is
        // a floor for the owner — external entries already admitted are never
        // evicted, and a new owner entry can only consume capacity that is
        // free right now.
        if external {
            let org_live = st.org_live(principal.acting_org);
            if org_live >= self.config.max_entries_per_external_org {
                st.reclaim_all(now);
                if st.org_live(principal.acting_org) >= self.config.max_entries_per_external_org {
                    self.per_org_denials.fetch_add(1, Ordering::Relaxed);
                    return ReplayOutcome::PerOrganizationCapacityExhausted;
                }
            }
            let external_pool = self
                .config
                .max_entries
                .saturating_sub(self.config.owner_reserved_entries);
            if st.external_total >= external_pool {
                st.reclaim_all(now);
                if st.external_total >= external_pool {
                    self.external_pool_denials.fetch_add(1, Ordering::Relaxed);
                    return ReplayOutcome::ExternalPoolCapacityExhausted;
                }
            }
        }

        // Global ceiling. Reclaim EXPIRED slots fleet-wide; if none
        // are reclaimable, deny fail-closed rather than evict a live
        // guard.
        //
        // Still reachable for OWNER traffic, which is bounded only here and
        // per-identity — an owner org with enough distinct identities can
        // legitimately fill the map, and denying fail-closed remains correct.
        // External traffic can no longer reach it: the pool bound above is
        // strictly tighter.
        if st.total >= self.config.max_entries {
            st.reclaim_all(now);
            if st.total >= self.config.max_entries {
                self.capacity_denials.fetch_add(1, Ordering::Relaxed);
                return ReplayOutcome::CapacityExhausted;
            }
        }

        st.by_caller.entry(caller.clone()).or_default().insert(
            call_id,
            ReplayEntry {
                binding_digest,
                expires_at,
                acting_org: *principal.acting_org,
                external,
            },
        );
        st.charge(external, principal.acting_org);
        ReplayOutcome::Admitted
    }

    /// Reclaim every entry whose window has closed as of `now`.
    /// Optional maintenance — [`Self::admit`] reclaims lazily at
    /// capacity — but a periodic sweep keeps steady-state memory
    /// low. Returns how many entries were reclaimed.
    pub fn evict_expired(&self, now: Instant) -> usize {
        self.entries.lock().reclaim_all(now)
    }

    /// Current tracked-entry count across all callers (test/metric
    /// surface).
    pub fn len(&self) -> usize {
        self.entries.lock().total
    }

    /// `true` iff no entries are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().total == 0
    }

    /// Number of entries currently tracked for one caller
    /// (test/metric surface).
    pub fn caller_len(&self, caller: &EntityId) -> usize {
        self.entries
            .lock()
            .by_caller
            .get(caller)
            .map_or(0, HashMap::len)
    }

    /// Total admissions denied for GLOBAL capacity since construction.
    pub fn capacity_denials(&self) -> u64 {
        self.capacity_denials.load(Ordering::Relaxed)
    }

    /// Live entries charged to one acting org, across ALL of its member
    /// identities (§5 test/metric surface).
    pub fn org_len(&self, org: &OrgId) -> usize {
        self.entries.lock().org_live(org)
    }

    /// Live entries drawing on the EXTERNAL pool (§5 test/metric surface).
    pub fn external_len(&self) -> usize {
        self.entries.lock().external_total
    }

    /// Total admissions denied because one EXTERNAL ORG exhausted its
    /// aggregate allocation (§5). Operators should read a rising value here as
    /// "one grantee is misbehaving", distinct from
    /// [`Self::capacity_denials`] ("the whole guard is under pressure").
    pub fn per_org_denials(&self) -> u64 {
        self.per_org_denials.load(Ordering::Relaxed)
    }

    /// Total admissions denied because the EXTERNAL POOL filled with no single
    /// org over quota (§5) — many active external orgs, owner org unaffected.
    pub fn external_pool_denials(&self) -> u64 {
        self.external_pool_denials.load(Ordering::Relaxed)
    }

    /// Total admissions denied for PER-CALLER capacity (E1.5) since
    /// construction.
    pub fn per_caller_denials(&self) -> u64 {
        self.per_caller_denials.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for AdmissionReplayGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionReplayGuard")
            .field("entries", &self.len())
            .field("max_entries", &self.config.max_entries)
            .field(
                "max_entries_per_caller",
                &self.config.max_entries_per_caller,
            )
            .field("capacity_denials", &self.capacity_denials())
            .field("per_caller_denials", &self.per_caller_denials())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn caller(byte: u8) -> EntityId {
        EntityId::from_bytes([byte; 32])
    }

    /// The provider's OWN org, for every pre-§5 witness.
    fn owner_org() -> OrgId {
        OrgId([0xAA; 32])
    }

    /// A distinct external org.
    fn external_org(byte: u8) -> OrgId {
        OrgId([byte; 32])
    }

    /// Pre-§5 shim: admit as the provider's OWN org.
    ///
    /// The existing witnesses are about replay / collision / per-caller
    /// ceilings — none is about trust-domain partitioning — so charging them
    /// to the owner org keeps each one testing exactly what it always did,
    /// rather than silently acquiring a second reason to fail.
    fn admit_owner(
        guard: &AdmissionReplayGuard,
        caller: &EntityId,
        call_id: u64,
        digest: [u8; 32],
        expires: Instant,
        now: Instant,
    ) -> ReplayOutcome {
        let owner = owner_org();
        guard.admit(
            ReplayPrincipal {
                caller,
                acting_org: &owner,
                provider_owner_org: &owner,
            },
            call_id,
            digest,
            expires,
            now,
        )
    }

    /// Admit as a member identity of `org`, an EXTERNAL org.
    fn admit_external(
        guard: &AdmissionReplayGuard,
        org: &OrgId,
        caller: &EntityId,
        call_id: u64,
        digest: [u8; 32],
        expires: Instant,
        now: Instant,
    ) -> ReplayOutcome {
        let owner = owner_org();
        guard.admit(
            ReplayPrincipal {
                caller,
                acting_org: org,
                provider_owner_org: &owner,
            },
            call_id,
            digest,
            expires,
            now,
        )
    }

    // ==================================================================
    // §6 — per-peer throttle on compelled signature work.
    // ==================================================================

    fn limiter() -> AdmissionFailureLimiter {
        AdmissionFailureLimiter::try_new(AdmissionRateLimitConfig {
            max_failed_per_peer: 4,
            refill_per_sec: 2,
            max_tracked_peers: 8,
        })
        .expect("valid envelope")
    }

    /// The property that makes this design defensible: an honest caller is
    /// COMPLETELY unaffected, however fast it calls.
    ///
    /// A per-attempt limiter would throttle legitimate traffic, and picking a
    /// rate that suits every deployment is exactly the guess that makes such
    /// limits wrong. Charging on FAILURE instead keys on the attacker's actual
    /// distinguishing property — its admissions fail; a real caller's succeed.
    #[test]
    fn a_peer_whose_admissions_succeed_is_never_throttled() {
        let lim = limiter();
        let now = Instant::now();
        const PEER: u64 = 7;

        // Far more attempts than the burst allowance, none of them failures.
        for _ in 0..1_000 {
            assert!(
                lim.may_attempt(PEER, now),
                "a successful caller must never be throttled — charging per \
                 ATTEMPT would penalise exactly the traffic we want",
            );
        }
        assert_eq!(lim.throttled_denials(), 0);
        assert_eq!(
            lim.tracked_peers(),
            0,
            "a peer with no failures costs nothing to track"
        );
    }

    /// A failing peer spends its burst and is then refused BEFORE the
    /// signature work — which is the resource being protected.
    #[test]
    fn a_failing_peer_exhausts_its_budget_and_is_refused() {
        let lim = limiter();
        let now = Instant::now();
        const PEER: u64 = 9;

        for _ in 0..4 {
            assert!(lim.may_attempt(PEER, now));
            lim.on_failure(PEER, now);
        }
        assert_eq!(lim.tokens_for(PEER), 0);
        assert!(
            !lim.may_attempt(PEER, now),
            "the peer must be refused once its failure budget is spent",
        );
        assert_eq!(lim.throttled_denials(), 1);
    }

    /// Throttling is per PEER: one abusive session must not deny another.
    #[test]
    fn throttling_one_peer_leaves_others_untouched() {
        let lim = limiter();
        let now = Instant::now();
        const NOISY: u64 = 1;
        const QUIET: u64 = 2;

        for _ in 0..4 {
            lim.on_failure(NOISY, now);
        }
        assert!(!lim.may_attempt(NOISY, now));
        assert!(
            lim.may_attempt(QUIET, now),
            "a second peer's allowance must be untouched by the first's abuse",
        );
    }

    /// The budget refills, so a legitimately misconfigured client recovers
    /// without operator intervention. A zero-refill envelope would make the
    /// first burst permanent, which `validate` refuses for this reason.
    #[test]
    fn the_budget_refills_over_time() {
        let lim = limiter();
        let t0 = Instant::now();
        const PEER: u64 = 3;

        for _ in 0..4 {
            lim.on_failure(PEER, t0);
        }
        assert!(!lim.may_attempt(PEER, t0));

        // 2 tokens/sec: one second restores two attempts.
        let later = t0 + Duration::from_secs(1);
        assert!(
            lim.may_attempt(PEER, later),
            "a throttled peer must recover on its own — otherwise one expired \
             proof at startup takes a client out until restart",
        );
        assert_eq!(lim.tokens_for(PEER), 2);

        // Refill saturates at the burst ceiling rather than growing unbounded.
        let much_later = t0 + Duration::from_secs(3_600);
        lim.may_attempt(PEER, much_later);
        assert_eq!(lim.tokens_for(PEER), 4);
    }

    /// The tracking map is bounded, so the limiter cannot become the memory
    /// exhaustion it exists to prevent.
    #[test]
    fn tracked_peers_are_bounded() {
        let lim = limiter();
        let now = Instant::now();
        for peer in 0..64u64 {
            lim.on_failure(peer, now + Duration::from_millis(peer));
        }
        assert!(
            lim.tracked_peers() <= 8,
            "peer tracking must stay within max_tracked_peers, got {}",
            lim.tracked_peers(),
        );
    }

    /// A degenerate envelope is refused loudly. Zero refill in particular
    /// would make the first burst permanent.
    #[test]
    fn a_degenerate_rate_limit_envelope_is_refused() {
        assert!(AdmissionRateLimitConfig {
            max_failed_per_peer: 0,
            refill_per_sec: 1,
            max_tracked_peers: 8,
        }
        .validate()
        .is_err());
        assert!(AdmissionRateLimitConfig {
            max_failed_per_peer: 4,
            refill_per_sec: 0,
            max_tracked_peers: 8,
        }
        .validate()
        .is_err());
        assert!(AdmissionRateLimitConfig::default().validate().is_ok());
    }
    // ==================================================================
    // §5 — trust-domain partitioning of the replay budget.
    //
    // The per-caller ceiling is correct in isolation and does not COMPOSE:
    // `max_entries / max_entries_per_caller == 16`, so sixteen identities
    // saturate the global map and the provider then denies its OWN owner-org
    // callers. Minting sixteen identities is one org-admin action, which is
    // what made the coalition cheap. Raising `max_entries` would only change
    // the coalition size; these tests pin the partition instead.
    // ==================================================================

    /// A tiny envelope with the same SHAPE as the shipped default
    /// (reserve < total, per-org <= external pool), so the properties are
    /// exercised without allocating 65 536 entries per test.
    ///
    /// `max_entries_per_caller` is deliberately BELOW the org quota, so
    /// filling an org's allocation requires a COALITION of identities — the
    /// realistic shape, and the one the attack exploits. With per-caller equal
    /// to per-org a single identity would trip the caller ceiling first and
    /// the org quota would never be the binding constraint under test.
    fn partitioned() -> AdmissionReplayGuard {
        AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 40,
            owner_reserved_entries: 10, // external pool = 30
            max_entries_per_external_org: 8,
            max_entries_per_caller: 4, // so one org needs 2 identities
        })
    }

    /// Fill `org` with up to `identities * 4` entries, using distinct member
    /// identities — the coalition shape. Returns (admitted, per-org denials).
    fn flood_org(
        guard: &AdmissionReplayGuard,
        org: &OrgId,
        identities: impl IntoIterator<Item = u8>,
        expires: Instant,
        now: Instant,
    ) -> (usize, usize) {
        let (mut admitted, mut denied) = (0usize, 0usize);
        for identity in identities {
            for call in 0..4u64 {
                match admit_external(
                    guard,
                    org,
                    &caller(identity),
                    call + u64::from(identity) * 1_000,
                    [identity; 32],
                    expires,
                    now,
                ) {
                    ReplayOutcome::Admitted => admitted += 1,
                    ReplayOutcome::PerOrganizationCapacityExhausted => denied += 1,
                    ReplayOutcome::ExternalPoolCapacityExhausted => denied += 1,
                    other => panic!("unexpected outcome {other:?}"),
                }
            }
        }
        (admitted, denied)
    }

    /// Witness 1 — sixteen identities from ONE external org collectively stop
    /// at the org quota, not at sixteen times the per-caller quota.
    ///
    /// This is the reported attack, scaled down: the coalition shares ONE
    /// allocation because the quota is keyed on the verified acting org.
    #[test]
    fn many_identities_from_one_external_org_share_one_allocation() {
        let guard = partitioned();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let org = external_org(0xB1);

        let (admitted, denied) = flood_org(&guard, &org, 0..16u8, expires, now);

        assert_eq!(
            admitted, 8,
            "sixteen identities from one org must share the SINGLE 8-entry org \
             allocation — if each got its own, the coalition wins",
        );
        assert!(denied > 0);
        assert_eq!(guard.org_len(&org), 8);
        assert_eq!(
            guard.per_org_denials(),
            denied as u64,
            "the per-ORG denial metric must fire, so an operator can attribute \
             this to one grantee rather than to fleet-wide pressure",
        );
        assert_eq!(
            guard.per_caller_denials(),
            0,
            "not a per-caller denial: naming a single identity would point the \
             operator at the wrong subject, since identities are free to mint",
        );
    }

    /// Witness 2 — filling the ENTIRE external pool cannot deny a fresh
    /// owner-org call. The headline property.
    #[test]
    fn a_full_external_pool_never_denies_the_owner_org() {
        let guard = partitioned();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        // Enough distinct orgs (2 identities each) that the POOL, not any one
        // org quota, is what stops them.
        let mut external_admitted = 0usize;
        for org_byte in 0..8u8 {
            let org = external_org(0xC0 + org_byte);
            let (a, _) = flood_org(
                &guard,
                &org,
                [org_byte * 2 + 100, org_byte * 2 + 101],
                expires,
                now,
            );
            external_admitted += a;
        }
        assert_eq!(
            external_admitted, 30,
            "external traffic must be capped at max_entries - owner_reserved",
        );
        assert_eq!(guard.external_len(), 30);

        assert_eq!(
            admit_external(
                &guard,
                &external_org(0xFE),
                &caller(99),
                1,
                [9u8; 32],
                expires,
                now
            ),
            ReplayOutcome::ExternalPoolCapacityExhausted,
        );

        assert_eq!(
            admit_owner(&guard, &caller(200), 1, [7u8; 32], expires, now),
            ReplayOutcome::Admitted,
            "an external coalition of ANY size must never deny the provider's \
             own org — this is the entire point of the reserve",
        );
    }

    /// Witness 3 — owner traffic may BORROW unused external capacity, so the
    /// partition costs nothing when there is no external load.
    #[test]
    fn owner_traffic_borrows_idle_external_capacity() {
        let guard = partitioned();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        // No external traffic at all: the owner should reach the GLOBAL cap
        // (40), not stop at its 10-entry reserve. 10 identities x 4 each.
        let mut admitted = 0usize;
        for identity in 0..10u8 {
            for call in 0..4u64 {
                if admit_owner(
                    &guard,
                    &caller(identity),
                    call,
                    [identity; 32],
                    expires,
                    now,
                ) == ReplayOutcome::Admitted
                {
                    admitted += 1;
                }
            }
        }
        assert_eq!(
            admitted, 40,
            "owner traffic must borrow idle external capacity up to the global \
             cap; stopping at the 10-entry reserve would make the partition a \
             throughput regression rather than a safety property",
        );
        assert_eq!(guard.len(), 40);
    }

    /// Witness 4 — distinct external orgs have INDEPENDENT allocations. One
    /// abusive grantee must not spend another grantee's quota.
    #[test]
    fn distinct_external_orgs_have_independent_allocations() {
        let guard = partitioned();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let noisy = external_org(0xB1);
        let quiet = external_org(0xB2);

        let (admitted, _) = flood_org(&guard, &noisy, [1u8, 11u8], expires, now);
        assert_eq!(admitted, 8);
        assert_eq!(
            admit_external(&guard, &noisy, &caller(21), 0, [21u8; 32], expires, now),
            ReplayOutcome::PerOrganizationCapacityExhausted,
            "a THIRD fresh identity must still be refused — the quota is the \
             org's, not the identity's",
        );

        let (quiet_admitted, quiet_denied) = flood_org(&guard, &quiet, [2u8, 12u8], expires, now);
        assert_eq!(
            quiet_admitted, 8,
            "a second org's quota must be untouched by the first's abuse",
        );
        assert_eq!(quiet_denied, 0);
        assert_eq!(guard.org_len(&noisy), 8);
        assert_eq!(guard.org_len(&quiet), 8);
    }

    /// Witness 5 — expiry reclamation decrements caller, ORG, external-pool
    /// and global counts in step.
    ///
    /// A counter that fails to decrement drifts upward forever and eventually
    /// denies a legitimate caller with no live entries to justify it — a leak
    /// that only shows up under sustained load, which is when it is hardest to
    /// diagnose. All four are asserted, before and after.
    #[test]
    fn reclamation_releases_every_counter_in_step() {
        let guard = partitioned();
        let t0 = Instant::now();
        let short = t0 + Duration::from_secs(5);
        let org = external_org(0xB1);

        let (admitted, _) = flood_org(&guard, &org, [1u8, 11u8], short, t0);
        assert_eq!(admitted, 8);
        assert_eq!(guard.len(), 8, "global");
        assert_eq!(guard.org_len(&org), 8, "per-org");
        assert_eq!(guard.external_len(), 8, "external pool");
        assert_eq!(guard.caller_len(&caller(1)), 4, "per-caller");

        let later = t0 + Duration::from_secs(6);
        assert_eq!(guard.evict_expired(later), 8);

        assert_eq!(guard.len(), 0, "global count leaked");
        assert_eq!(guard.org_len(&org), 0, "per-org count leaked");
        assert_eq!(guard.external_len(), 0, "external-pool count leaked");
        assert_eq!(guard.caller_len(&caller(1)), 0, "per-caller count leaked");

        assert_eq!(
            admit_external(
                &guard,
                &org,
                &caller(1),
                100,
                [1u8; 32],
                later + Duration::from_secs(30),
                later
            ),
            ReplayOutcome::Admitted,
            "a reclaimed org slot must be usable again, or the quota is a \
             one-way ratchet",
        );
    }

    /// Witness 6 — the three capacity outcomes are DISTINCT, so an operator
    /// can tell an abusive grantee from external saturation from true global
    /// exhaustion. Conflating them is what makes this class of incident
    /// unattributable.
    #[test]
    fn the_three_capacity_outcomes_are_distinguishable() {
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        // (a) one org over ITS quota; pool and global both fine.
        let guard = partitioned();
        let org = external_org(0xB1);
        flood_org(&guard, &org, [1u8, 11u8], expires, now);
        assert_eq!(
            admit_external(&guard, &org, &caller(21), 0, [21u8; 32], expires, now),
            ReplayOutcome::PerOrganizationCapacityExhausted,
        );
        assert_eq!(guard.per_org_denials(), 1);
        assert_eq!(guard.external_pool_denials(), 0);
        assert_eq!(guard.capacity_denials(), 0);

        // (b) external pool full, no single org over quota.
        let guard = partitioned();
        for org_byte in 0..8u8 {
            let org = external_org(0xC0 + org_byte);
            flood_org(
                &guard,
                &org,
                [org_byte * 2 + 100, org_byte * 2 + 101],
                expires,
                now,
            );
        }
        assert_eq!(
            admit_external(
                &guard,
                &external_org(0xFE),
                &caller(99),
                1,
                [9u8; 32],
                expires,
                now
            ),
            ReplayOutcome::ExternalPoolCapacityExhausted,
        );
        assert!(guard.external_pool_denials() >= 1);
        assert_eq!(
            guard.capacity_denials(),
            0,
            "external saturation is NOT global exhaustion — the owner reserve \
             is free by construction, so reporting it as global would send an \
             operator looking for fleet-wide pressure that does not exist",
        );

        // (c) true global exhaustion, reachable only by OWNER traffic.
        let guard = partitioned();
        for identity in 0..10u8 {
            for call in 0..4u64 {
                admit_owner(
                    &guard,
                    &caller(identity),
                    call,
                    [identity; 32],
                    expires,
                    now,
                );
            }
        }
        assert_eq!(guard.len(), 40);
        assert_eq!(
            admit_owner(&guard, &caller(50), 1, [50u8; 32], expires, now),
            ReplayOutcome::CapacityExhausted,
        );
        assert_eq!(guard.capacity_denials(), 1);
        assert_eq!(guard.per_org_denials(), 0);
        assert_eq!(guard.external_pool_denials(), 0);
    }

    /// The envelope invariants are refused loudly, not clamped: a reserve at
    /// or above the total would leave external callers nothing, and a per-org
    /// quota above the external pool would make the pool bound unreachable.
    #[test]
    fn an_inconsistent_envelope_is_refused() {
        let reserve_too_big = AdmissionReplayConfig {
            max_entries: 100,
            owner_reserved_entries: 100,
            max_entries_per_external_org: 10,
            max_entries_per_caller: 10,
        };
        assert!(matches!(
            reserve_too_big.validate(),
            Err(ReplayConfigError::OwnerReserveNotBelowGlobal { .. })
        ));

        let org_quota_too_big = AdmissionReplayConfig {
            max_entries: 100,
            owner_reserved_entries: 95, // external pool = 5
            max_entries_per_external_org: 10,
            max_entries_per_caller: 10,
        };
        assert!(matches!(
            org_quota_too_big.validate(),
            Err(ReplayConfigError::PerExternalOrgAboveExternalPool { .. })
        ));

        // The shipped default must itself be valid.
        assert!(AdmissionReplayConfig::default().validate().is_ok());
    }

    #[test]
    fn first_admit_records_and_replay_is_denied() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Same proof re-presented within the window: replay.
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, expires, now),
            ReplayOutcome::Replay
        );
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn same_call_id_different_binding_is_a_collision() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        assert_eq!(
            admit_owner(&guard, &caller(1), 7, [1u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, [2u8; 32], expires, now),
            ReplayOutcome::CallIdCollision
        );
    }

    #[test]
    fn distinct_callers_and_call_ids_are_independent() {
        let guard = AdmissionReplayGuard::with_defaults();
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Different call_id, same caller: independent.
        assert_eq!(
            admit_owner(&guard, &caller(1), 8, digest, expires, now),
            ReplayOutcome::Admitted
        );
        // Different caller, same call_id: independent.
        assert_eq!(
            admit_owner(&guard, &caller(2), 7, digest, expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.len(), 3);
    }

    #[test]
    fn expired_entry_permits_legitimate_call_id_reuse() {
        let guard = AdmissionReplayGuard::with_defaults();
        let t0 = Instant::now();
        let expires = t0 + Duration::from_secs(30);
        let digest = [1u8; 32];

        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, expires, t0),
            ReplayOutcome::Admitted
        );
        // Same key AFTER the window closes is a fresh, legitimate
        // call — not a replay.
        let later = t0 + Duration::from_secs(31);
        let new_expires = later + Duration::from_secs(30);
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, new_expires, later),
            ReplayOutcome::Admitted
        );
        // And the SAME proof within the NEW window is a replay again.
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, digest, new_expires, later),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn capacity_denies_without_evicting_a_live_guard() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 2,
            max_entries_per_caller: 1,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 2,
        });
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        assert_eq!(
            admit_owner(&guard, &caller(1), 1, [1u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            admit_owner(&guard, &caller(2), 2, [2u8; 32], expires, now),
            ReplayOutcome::Admitted
        );
        // Full of LIVE entries: a novel admission is denied, and
        // the metric ticks — no live guard is dropped.
        assert_eq!(
            admit_owner(&guard, &caller(3), 3, [3u8; 32], expires, now),
            ReplayOutcome::CapacityExhausted
        );
        assert_eq!(guard.capacity_denials(), 1);
        assert_eq!(guard.len(), 2);
        // The still-live originals remain protected.
        assert_eq!(
            admit_owner(&guard, &caller(1), 1, [1u8; 32], expires, now),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn capacity_reclaims_expired_slots_before_denying() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 2,
            max_entries_per_caller: 1,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 2,
        });
        let t0 = Instant::now();
        let short = t0 + Duration::from_secs(10);
        let long = t0 + Duration::from_secs(60);

        assert_eq!(
            admit_owner(&guard, &caller(1), 1, [1u8; 32], short, t0),
            ReplayOutcome::Admitted
        );
        assert_eq!(
            admit_owner(&guard, &caller(2), 2, [2u8; 32], long, t0),
            ReplayOutcome::Admitted
        );
        // After caller(1)'s window closes, a novel admission at
        // capacity reclaims the expired slot instead of denying.
        let later = t0 + Duration::from_secs(11);
        assert_eq!(
            admit_owner(
                &guard,
                &caller(3),
                3,
                [3u8; 32],
                later + Duration::from_secs(30),
                later
            ),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.capacity_denials(), 0);
        // caller(2) (long window) survived; caller(1) was reclaimed.
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn evict_expired_reclaims_only_closed_windows() {
        let guard = AdmissionReplayGuard::with_defaults();
        let t0 = Instant::now();
        admit_owner(
            &guard,
            &caller(1),
            1,
            [1u8; 32],
            t0 + Duration::from_secs(10),
            t0,
        );
        admit_owner(
            &guard,
            &caller(2),
            2,
            [2u8; 32],
            t0 + Duration::from_secs(60),
            t0,
        );

        let reclaimed = guard.evict_expired(t0 + Duration::from_secs(11));
        assert_eq!(reclaimed, 1);
        assert_eq!(guard.len(), 1);
        // The unexpired one is untouched — still a replay.
        assert_eq!(
            admit_owner(
                &guard,
                &caller(2),
                2,
                [2u8; 32],
                t0 + Duration::from_secs(60),
                t0 + Duration::from_secs(11)
            ),
            ReplayOutcome::Replay
        );
    }

    #[test]
    fn concurrent_admissions_admit_exactly_once() {
        use std::sync::Arc;
        let guard = Arc::new(AdmissionReplayGuard::with_defaults());
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);
        let digest = [7u8; 32];

        let admitted = Arc::new(AtomicU64::new(0));
        let replayed = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let guard = guard.clone();
            let admitted = admitted.clone();
            let replayed = replayed.clone();
            handles.push(std::thread::spawn(move || {
                match admit_owner(&guard, &caller(1), 42, digest, expires, now) {
                    ReplayOutcome::Admitted => {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                    ReplayOutcome::Replay => {
                        replayed.fetch_add(1, Ordering::Relaxed);
                    }
                    other => panic!("unexpected outcome {other:?}"),
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }
        assert_eq!(admitted.load(Ordering::Relaxed), 1, "exactly one admit");
        assert_eq!(replayed.load(Ordering::Relaxed), 15, "the rest replay");
    }

    /// E1.5 witness 21 — one caller cannot consume another's replay
    /// allocation. Caller(1) fills its per-caller ceiling; a further
    /// NOVEL call from caller(1) is denied `PerCallerCapacityExhausted`,
    /// yet caller(2) admits freely and the GLOBAL capacity denial
    /// metric never ticks.
    #[test]
    fn per_caller_ceiling_isolates_a_flooding_caller() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 1_000,
            max_entries_per_caller: 3,
            // Pre-§5 shape: no owner reserve, non-binding org quota.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 1_000,
        });
        let now = Instant::now();
        let expires = now + Duration::from_secs(30);

        // caller(1) fills its per-caller allocation with 3 novel calls.
        for call_id in 0..3u64 {
            assert_eq!(
                admit_owner(
                    &guard,
                    &caller(1),
                    call_id,
                    [call_id as u8; 32],
                    expires,
                    now
                ),
                ReplayOutcome::Admitted
            );
        }
        assert_eq!(guard.caller_len(&caller(1)), 3);

        // The 4th novel call from caller(1) is denied — only caller(1).
        assert_eq!(
            admit_owner(&guard, &caller(1), 99, [9u8; 32], expires, now),
            ReplayOutcome::PerCallerCapacityExhausted
        );
        assert_eq!(guard.per_caller_denials(), 1);
        assert_eq!(guard.capacity_denials(), 0, "global cap never fired");

        // caller(2) is entirely unaffected — its allocation is its own.
        for call_id in 0..3u64 {
            assert_eq!(
                admit_owner(
                    &guard,
                    &caller(2),
                    call_id,
                    [call_id as u8; 32],
                    expires,
                    now
                ),
                ReplayOutcome::Admitted
            );
        }
        assert_eq!(guard.caller_len(&caller(2)), 3);
        // A still-live replay from caller(1) is unchanged behavior.
        assert_eq!(
            admit_owner(&guard, &caller(1), 0, [0u8; 32], expires, now),
            ReplayOutcome::Replay
        );
    }

    /// The per-caller ceiling reclaims that caller's EXPIRED slots
    /// before denying, so a caller whose earlier calls have aged out
    /// can keep making new ones without ever touching other callers.
    #[test]
    fn per_caller_ceiling_reclaims_expired_before_denying() {
        let guard = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 1_000,
            max_entries_per_caller: 2,
            // Pre-§5 shape: no owner reserve, non-binding org quota.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 1_000,
        });
        let t0 = Instant::now();
        let short = t0 + Duration::from_secs(10);

        admit_owner(&guard, &caller(1), 1, [1u8; 32], short, t0);
        admit_owner(&guard, &caller(1), 2, [2u8; 32], short, t0);
        assert_eq!(guard.caller_len(&caller(1)), 2);

        // After caller(1)'s window closes, a novel call at the
        // per-caller cap reclaims the expired slots instead of denying.
        let later = t0 + Duration::from_secs(11);
        assert_eq!(
            admit_owner(
                &guard,
                &caller(1),
                3,
                [3u8; 32],
                later + Duration::from_secs(30),
                later
            ),
            ReplayOutcome::Admitted
        );
        assert_eq!(guard.per_caller_denials(), 0);
        assert_eq!(guard.caller_len(&caller(1)), 1, "expired slots reclaimed");
    }

    /// KC8 — config validation boundaries (Kyra E1 audit). The
    /// invariant is `0 < max_entries_per_caller < max_entries`, loud
    /// via `try_new`, so no config can let one caller consume the
    /// whole global guard.
    #[test]
    fn replay_config_validation_boundaries() {
        // Valid: strictly below.
        assert!(AdmissionReplayConfig {
            max_entries: 10,
            max_entries_per_caller: 9,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 10,
        }
        .validate()
        .is_ok());
        assert!(AdmissionReplayGuard::try_new(AdmissionReplayConfig {
            max_entries: 10,
            max_entries_per_caller: 9,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 10,
        })
        .is_ok());
        // Defaults are valid.
        assert!(AdmissionReplayConfig::default().validate().is_ok());

        // per_caller == max_entries → rejected.
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 8,
                max_entries_per_caller: 8,
                // Pre-§5 shape: no owner reserve and a non-binding org quota, so
                // these witnesses keep testing ONLY the global and per-caller
                // ceilings they were written for rather than acquiring a second,
                // unrelated reason to deny.
                owner_reserved_entries: 0,
                max_entries_per_external_org: 8,
            }
            .validate(),
            Err(ReplayConfigError::PerCallerNotBelowGlobal {
                per_caller: 8,
                global: 8,
            }),
        );
        // per_caller > max_entries → rejected.
        assert!(matches!(
            AdmissionReplayConfig {
                max_entries: 8,
                max_entries_per_caller: 9,
                // Pre-§5 shape: no owner reserve and a non-binding org quota, so
                // these witnesses keep testing ONLY the global and per-caller
                // ceilings they were written for rather than acquiring a second,
                // unrelated reason to deny.
                owner_reserved_entries: 0,
                max_entries_per_external_org: 8,
            }
            .validate(),
            Err(ReplayConfigError::PerCallerNotBelowGlobal { .. }),
        ));
        // Zero ceilings → rejected.
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 0,
                max_entries_per_caller: 0,
                // Pre-§5 shape: no owner reserve and a non-binding org quota, so
                // these witnesses keep testing ONLY the global and per-caller
                // ceilings they were written for rather than acquiring a second,
                // unrelated reason to deny.
                owner_reserved_entries: 0,
                max_entries_per_external_org: 0,
            }
            .validate(),
            Err(ReplayConfigError::ZeroGlobalCeiling),
        );
        assert_eq!(
            AdmissionReplayConfig {
                max_entries: 4,
                max_entries_per_caller: 0,
                // Pre-§5 shape: no owner reserve and a non-binding org quota, so
                // these witnesses keep testing ONLY the global and per-caller
                // ceilings they were written for rather than acquiring a second,
                // unrelated reason to deny.
                owner_reserved_entries: 0,
                max_entries_per_external_org: 4,
            }
            .validate(),
            Err(ReplayConfigError::ZeroPerCallerCeiling),
        );
        // try_new surfaces the error rather than clamping.
        assert!(AdmissionReplayGuard::try_new(AdmissionReplayConfig {
            max_entries: 8,
            max_entries_per_caller: 8,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 8,
        })
        .is_err());
    }

    /// The loud `new` panics on an invalid config (never clamps).
    #[test]
    #[should_panic(expected = "invalid AdmissionReplayConfig")]
    fn replay_new_panics_on_invalid_config() {
        let _ = AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 4,
            max_entries_per_caller: 4,
            // Pre-§5 shape: no owner reserve and a non-binding org quota, so
            // these witnesses keep testing ONLY the global and per-caller
            // ceilings they were written for rather than acquiring a second,
            // unrelated reason to deny.
            owner_reserved_entries: 0,
            max_entries_per_external_org: 4,
        });
    }
}
