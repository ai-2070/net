//! The volatile admission replay guard and the per-peer failure
//! limiter.
//!
//! Core's `behavior/org_admission_replay.rs`, ported for a
//! single-threaded, runtime-free leaf: `parking_lot::Mutex` becomes
//! `core::cell::RefCell`, the `AtomicU64` metrics become `Cell<u64>`,
//! and `std::time::Instant` becomes **u64 monotonic milliseconds**
//! passed by the caller (the guard never reads a clock). `admit` is
//! atomic insert-or-deny: one borrow covers eviction, the collision
//! check and the insert, so two presentations of one proof can never
//! both see "absent" and both admit (there is no preemption here to
//! make the interleaving real, but the invariant must not depend on
//! that).
//!
//! # Why the quotas are shaped like this
//!
//! A single caller could fill the whole global map and starve every
//! other org fail-closed, so there is a per-caller sub-ceiling
//! checked FIRST. Minting identities is one org-admin action, so
//! quotas are keyed on the VERIFIED acting org — sixteen identities
//! from one grantee org share ONE allocation. The provider's own org
//! gets a reserve no external caller can touch, and may borrow idle
//! external capacity. Reclamation only ever drops EXPIRED slots — a
//! live guard is never evicted; exhaustion denies fail-closed with a
//! distinguishable metric.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::org::cert::OrgId;
use crate::org::entity::EntityId;

/// Ceiling on tracked in-flight+recent admissions. Sized so a burst
/// of legitimate concurrent callers fits comfortably while a single
/// caller cannot exhaust memory with novel `call_id`s.
pub const DEFAULT_MAX_REPLAY_ENTRIES: usize = 65_536;

/// Per-caller ceiling. Policy runs AFTER replay insertion, so even a
/// policy-vetoed VALID proof consumes a slot; without a per-caller
/// sub-ceiling a single credentialed caller could fill the whole
/// global map and starve every other org fail-closed.
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER: usize = 4_096;

/// Entries within [`DEFAULT_MAX_REPLAY_ENTRIES`] reserved for the
/// PROVIDER'S OWN owner org. External callers can never consume
/// this reserve, so no external coalition of any size can deny the
/// provider's own org.
pub const DEFAULT_OWNER_RESERVED_REPLAY_ENTRIES: usize = 16_384;

/// Aggregate ceiling for ONE external acting organization, across
/// ALL of its member identities — the quota that defeats a coalition
/// of cheaply-minted identities. Keyed on the VERIFIED acting org —
/// never the issuer, the peer/session, or any claimed wire field.
pub const DEFAULT_MAX_REPLAY_ENTRIES_PER_EXTERNAL_ORG: usize = 4_096;

/// Replay-guard ceilings — a global map cap plus the per-caller and
/// per-external-org sub-ceilings and the owner reserve.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionReplayConfig {
    /// Maximum simultaneously-retained `(caller, call_id)` entries
    /// across ALL callers. At capacity, a novel admission denies
    /// rather than evicting an unexpired guard.
    pub max_entries: usize,
    /// Maximum simultaneously-retained entries for ONE caller.
    /// Checked before the global cap, so a flooding caller hits its
    /// own ceiling first and never denies other callers.
    pub max_entries_per_caller: usize,
    /// Entries within [`Self::max_entries`] reserved for the
    /// provider's OWN owner org, which external callers can never
    /// consume.
    pub owner_reserved_entries: usize,
    /// Aggregate ceiling for ONE external acting org across all of
    /// its member identities. The provider's own owner org is
    /// deliberately NOT subject to this.
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
    /// Enforce the ceiling invariant: all bounds positive, the
    /// per-caller ceiling STRICTLY below the global one (a caller
    /// that could fill the entire guard would starve every other
    /// org), the owner reserve strictly below the global cap (or
    /// external callers would have no capacity at all), and the
    /// per-org quota fitting inside the external pool (or the pool
    /// bound would be unreachable and the org quota decorative).
    /// Validated loudly at construction rather than silently
    /// clamped.
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
        if self.owner_reserved_entries >= self.max_entries {
            return Err(ReplayConfigError::OwnerReserveNotBelowGlobal {
                reserved: self.owner_reserved_entries,
                global: self.max_entries,
            });
        }
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

/// An invalid [`AdmissionReplayConfig`] (or
/// [`AdmissionRateLimitConfig`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayConfigError {
    /// `max_entries == 0` — the global guard could never admit.
    ZeroGlobalCeiling,
    /// `max_entries_per_caller == 0` — no caller could ever admit.
    ZeroPerCallerCeiling,
    /// `max_entries_per_caller >= max_entries` — one caller could
    /// consume the entire global guard.
    PerCallerNotBelowGlobal {
        /// The configured per-caller ceiling.
        per_caller: usize,
        /// The configured global ceiling.
        global: usize,
    },
    /// `max_entries_per_external_org == 0` — no external org could
    /// admit.
    ZeroPerExternalOrgCeiling,
    /// `owner_reserved_entries >= max_entries` — external callers
    /// would have no capacity at all.
    OwnerReserveNotBelowGlobal {
        /// The configured owner reserve.
        reserved: usize,
        /// The configured global ceiling.
        global: usize,
    },
    /// The per-external-org quota cannot fit inside the external
    /// pool.
    PerExternalOrgAboveExternalPool {
        /// The configured per-external-org ceiling.
        per_org: usize,
        /// The derived external pool size.
        external_pool: usize,
    },
}

impl core::fmt::Display for ReplayConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroGlobalCeiling => write!(f, "replay max_entries must be > 0"),
            Self::ZeroPerCallerCeiling => write!(f, "replay max_entries_per_caller must be > 0"),
            Self::PerCallerNotBelowGlobal { per_caller, global } => write!(
                f,
                "replay max_entries_per_caller ({per_caller}) must be < max_entries ({global})"
            ),
            Self::ZeroPerExternalOrgCeiling => {
                write!(f, "replay max_entries_per_external_org must be > 0")
            }
            Self::OwnerReserveNotBelowGlobal { reserved, global } => write!(
                f,
                "replay owner_reserved_entries ({reserved}) must be < max_entries ({global}); \
                 a reserve at or above the global cap leaves external callers nothing"
            ),
            Self::PerExternalOrgAboveExternalPool {
                per_org,
                external_pool,
            } => write!(
                f,
                "replay max_entries_per_external_org ({per_org}) must be <= the external pool \
                 ({external_pool} = max_entries - owner_reserved_entries)"
            ),
        }
    }
}

impl std::error::Error for ReplayConfigError {}

/// The outcome of an admission check. Only [`Self::Admitted`] lets
/// the handler run; the §2.4 engine maps the others to typed
/// `AdmissionDenied` reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// First sight of this `(caller, call_id)` within its window —
    /// recorded; the handler may run.
    Admitted,
    /// The SAME proof (identical binding digest) re-presented before
    /// expiry — a replay.
    Replay,
    /// The same `(caller, call_id)` with a DIFFERENT binding digest
    /// — a correlation-id collision (caller bug or a forged reuse of
    /// an id).
    CallIdCollision,
    /// The GLOBAL guard is full of still-live entries; admitting
    /// would require evicting an unexpired guard, so this call is
    /// denied fail-closed.
    CapacityExhausted,
    /// THIS caller already holds the maximum simultaneously-retained
    /// entries. Denies only this caller — every other caller's
    /// allocation is untouched.
    PerCallerCapacityExhausted,
    /// THIS external acting ORGANIZATION has consumed its aggregate
    /// allocation, across all of its member identities.
    PerOrganizationCapacityExhausted,
    /// The EXTERNAL pool is full of live entries, with no single org
    /// over its own quota — genuinely many active external orgs;
    /// notably NOT a state in which the provider's own org is
    /// affected.
    ExternalPoolCapacityExhausted,
}

/// The VERIFIED principal an admission is charged to.
///
/// A struct rather than loose arguments because WHICH identity each
/// quota is keyed on is the whole security property, and a
/// positional `&OrgId, &OrgId` pair would be trivial to transpose at
/// a call site.
#[derive(Debug, Clone, Copy)]
pub struct ReplayPrincipal<'a> {
    /// The caller entity, resolved from the AUTHENTICATED direct
    /// session — never a request-body field.
    pub caller: &'a EntityId,
    /// The org the caller is VERIFIED to be acting for: taken from
    /// the org-signed membership certificate and cross-checked
    /// against the dispatcher grant by `verify_org_admission`.
    /// Deliberately NOT the certificate ISSUER, the peer/session, or
    /// any claimed wire field.
    pub acting_org: &'a OrgId,
    /// The PROVIDER's own owner org — the beneficiary of the
    /// reserve.
    pub provider_owner_org: &'a OrgId,
}

impl ReplayPrincipal<'_> {
    /// Whether this admission is charged to the provider's own org,
    /// and so draws on the reserve rather than the external pool.
    fn is_owner_org(&self) -> bool {
        self.acting_org == self.provider_owner_org
    }
}

struct ReplayEntry {
    binding_digest: [u8; 32],
    /// Monotonic instant (u64 ms) at/after which this entry is
    /// reusable.
    expires_at: u64,
    /// The acting org this entry is charged to. Carried on the ENTRY
    /// so reclamation can decrement the org counter and the
    /// external-pool counter without re-deriving anything.
    acting_org: OrgId,
    /// Whether this entry drew on the external pool (i.e. was NOT
    /// owner-org). Cached rather than recomputed at reclaim time
    /// because the provider's owner org can CHANGE under a re-adopt;
    /// recomputing would then return a slot to the wrong pool and
    /// permanently skew the accounting.
    external: bool,
}

/// The borrow-guarded state. Nested `caller → (call_id → entry)` so
/// the per-caller ceiling and per-caller reclamation touch ONLY one
/// caller's entries; `total` mirrors the summed inner lengths so the
/// global cap is a field read, not an O(callers) sum.
#[derive(Default)]
struct ReplayState {
    by_caller: HashMap<EntityId, HashMap<u64, ReplayEntry>>,
    total: usize,
    /// Live entries per VERIFIED acting org — the aggregate quota.
    by_org: HashMap<OrgId, usize>,
    /// Live entries drawing on the EXTERNAL pool. Mirrors the summed
    /// external `by_org` values so the pool bound is a field read.
    external_total: usize,
}

impl ReplayState {
    /// Charge one entry to every counter it consumes. The four
    /// counters move together, under the caller's borrow, or not at
    /// all.
    fn charge(&mut self, entry_external: bool, acting_org: &OrgId) {
        self.total += 1;
        self.charge_quota(entry_external, acting_org);
    }

    /// Release one reclaimed entry from every counter it consumed —
    /// the mirror of [`Self::charge`]. Decrementing out of step
    /// leaks a quota upward forever and eventually denies a
    /// legitimate caller with no live entries to justify it.
    fn release(&mut self, entry: &ReplayEntry) {
        self.total -= 1;
        self.release_quota(entry);
    }

    /// Move one overwritten entry's quota onto the principal the
    /// re-admission presents — the expired-overwrite mirror of
    /// [`Self::release`] + [`Self::charge`] WITHOUT `total`, because
    /// the `(caller, call_id)` key was occupied and stays occupied.
    ///
    /// The CHARGE must move anyway: the key excludes `acting_org`,
    /// and this path runs only AFTER the window has expired, so the
    /// caller can return under a DIFFERENT verified acting org (or a
    /// re-adopt can flip [`ReplayEntry::external`]). Leaving the
    /// replaced entry's counters in place strands the old org's quota
    /// — denied [`ReplayOutcome::PerOrganizationCapacityExhausted`]
    /// with zero live entries — and makes `release` decrement
    /// counters the stored entry never incremented, underflowing
    /// `external_total` on an `external` flip.
    fn retarget(&mut self, old: &ReplayEntry, entry_external: bool, acting_org: &OrgId) {
        if old.acting_org != *acting_org || old.external != entry_external {
            self.release_quota(old);
            self.charge_quota(entry_external, acting_org);
        }
    }

    /// Charge one entry's per-org and external-pool share — every
    /// counter except `total`. Split out so [`Self::charge`] and
    /// [`Self::retarget`] share one implementation of the quota moves.
    fn charge_quota(&mut self, entry_external: bool, acting_org: &OrgId) {
        *self.by_org.entry(*acting_org).or_insert(0) += 1;
        if entry_external {
            self.external_total += 1;
        }
    }

    /// Release one entry's per-org and external-pool share — the
    /// mirror of [`Self::charge_quota`]. `external_total` is SATURATED
    /// rather than wrapped: symmetry keeps it from ever underflowing,
    /// and a wrap here would turn one accounting slip into a debug
    /// panic inside the admission path — or, in release, into
    /// `usize::MAX` and [`ReplayOutcome::ExternalPoolCapacityExhausted`]
    /// for every external caller, forever.
    fn release_quota(&mut self, entry: &ReplayEntry) {
        if let Some(count) = self.by_org.get_mut(&entry.acting_org) {
            *count -= 1;
            if *count == 0 {
                self.by_org.remove(&entry.acting_org);
            }
        }
        if entry.external {
            self.external_total = self.external_total.saturating_sub(1);
        }
    }

    /// Drop `caller`'s expired entries (and the caller bucket if it
    /// empties), releasing each from every counter it held.
    fn reclaim_caller(&mut self, caller: &EntityId, now: u64) {
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

    /// Drop every expired entry across all callers, releasing each
    /// from every counter it held. Returns the number reclaimed.
    fn reclaim_all(&mut self, now: u64) -> usize {
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

/// Burst of FAILED admissions one authenticated peer may cost before
/// it is throttled.
pub const DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER: u32 = 64;

/// Failed-admission budget refilled per second, per peer.
pub const DEFAULT_FAILED_ADMISSION_REFILL_PER_SEC: u32 = 8;

/// Maximum peers tracked at once; the oldest idle entry is reclaimed
/// at capacity. Bounded so the limiter cannot itself become the
/// memory exhaustion it exists to prevent.
pub const DEFAULT_MAX_RATE_LIMITED_PEERS: usize = 4_096;

/// Per-peer throttle on the SIGNATURE work a failing caller can
/// compel.
///
/// A peer needs only a TOFU-pinned session and NO org credentials to
/// reach the expensive part of the gate: it self-mints an org root,
/// issues itself genuinely valid certs and grants under that key,
/// and attaches a garbage capability grant — every cheap plaintext
/// check passes and the gate performs THREE `verify_strict`
/// operations before denying. Failed admissions consume no replay
/// slot (the guard records only ADMITTED calls), so the replay
/// ceilings never see this traffic.
///
/// The limiter charges on FAILURE only: a successful admission costs
/// nothing, so honest traffic never touches it, while a peer failing
/// faster than the refill is — by construction — broken or hostile,
/// and is refused BEFORE the signature work.
pub struct AdmissionFailureLimiter {
    buckets: RefCell<HashMap<u64, PeerBucket>>,
    config: AdmissionRateLimitConfig,
    /// Admissions refused because the peer's failure budget was
    /// exhausted.
    throttled: Cell<u64>,
}

/// Tunables for [`AdmissionFailureLimiter`].
#[derive(Debug, Clone, Copy)]
pub struct AdmissionRateLimitConfig {
    /// Burst of failed admissions one peer may cost before
    /// throttling.
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
    /// A zero refill would make the first burst permanent — one
    /// expired proof at startup would take a client out until
    /// restart.
    pub fn validate(&self) -> Result<(), ReplayConfigError> {
        if self.max_failed_per_peer == 0 {
            return Err(ReplayConfigError::ZeroPerCallerCeiling);
        }
        if self.refill_per_sec == 0 {
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
    /// When `tokens` was last refilled (u64 mono ms).
    last_refill: u64,
    /// Last touch, for idle reclamation at capacity (u64 mono ms).
    last_seen: u64,
}

impl AdmissionFailureLimiter {
    /// A limiter with the given envelope, VALIDATED.
    pub fn try_new(config: AdmissionRateLimitConfig) -> Result<Self, ReplayConfigError> {
        config.validate()?;
        Ok(Self {
            buckets: RefCell::new(HashMap::new()),
            config,
            throttled: Cell::new(0),
        })
    }

    /// A limiter with the default envelope (always valid).
    pub fn with_defaults() -> Self {
        Self {
            buckets: RefCell::new(HashMap::new()),
            config: AdmissionRateLimitConfig::default(),
            throttled: Cell::new(0),
        }
    }

    /// May `from_node` attempt an admission at monotonic instant
    /// `now` (u64 ms)? Call BEFORE the signature work. `false` means
    /// the peer has spent its failure budget and must be denied
    /// cheaply.
    pub fn may_attempt(&self, from_node: u64, now: u64) -> bool {
        let mut buckets = self.buckets.borrow_mut();
        let cfg = self.config;
        match buckets.get_mut(&from_node) {
            None => true, // unseen peer: no failures charged yet
            Some(bucket) => {
                Self::refill(bucket, cfg, now);
                bucket.last_seen = now;
                if bucket.tokens > 0 {
                    true
                } else {
                    self.throttled.set(self.throttled.get() + 1);
                    false
                }
            }
        }
    }

    /// Charge one failed admission to `from_node`. Called ONLY on a
    /// denial. At capacity the least-recently-seen peer is evicted —
    /// safe: losing a bucket only restores that peer's full
    /// allowance, and the evicted one is by definition the least
    /// active.
    pub fn on_failure(&self, from_node: u64, now: u64) {
        let mut buckets = self.buckets.borrow_mut();
        let cfg = self.config;
        if !buckets.contains_key(&from_node) {
            if buckets.len() >= cfg.max_tracked_peers {
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

    /// Refill by whole elapsed seconds (u64 mono ms), capped at the
    /// burst.
    fn refill(bucket: &mut PeerBucket, cfg: AdmissionRateLimitConfig, now: u64) {
        let secs = now.saturating_sub(bucket.last_refill) / 1000;
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

    /// Admissions refused because the peer's failure budget was
    /// exhausted.
    pub fn throttled_denials(&self) -> u64 {
        self.throttled.get()
    }

    /// Remaining failure allowance for `from_node` (test/metric
    /// surface).
    pub fn tokens_for(&self, from_node: u64) -> u32 {
        self.buckets
            .borrow()
            .get(&from_node)
            .map_or(self.config.max_failed_per_peer, |b| b.tokens)
    }

    /// Peers currently tracked (test/metric surface).
    pub fn tracked_peers(&self) -> usize {
        self.buckets.borrow().len()
    }
}

/// The volatile admission replay guard. One per provider node.
pub struct AdmissionReplayGuard {
    entries: RefCell<ReplayState>,
    config: AdmissionReplayConfig,
    /// Admissions denied for GLOBAL capacity.
    capacity_denials: Cell<u64>,
    /// Admissions denied for PER-CALLER capacity — a separate metric
    /// so operators can tell a fleet-wide flood from a single
    /// abusive caller.
    per_caller_denials: Cell<u64>,
    /// Admissions denied because ONE EXTERNAL ORG exhausted its
    /// aggregate allocation — the signal that identifies an abusive
    /// grantee.
    per_org_denials: Cell<u64>,
    /// Admissions denied because the EXTERNAL POOL was full with no
    /// single org over quota — many active external orgs, and NOT a
    /// state in which the provider's own org is affected.
    external_pool_denials: Cell<u64>,
}

impl AdmissionReplayGuard {
    /// A guard with the given ceilings, VALIDATED — see
    /// [`AdmissionReplayConfig::validate`]. Prefer this over
    /// [`Self::new`] on any config not known-good at compile time.
    pub fn try_new(config: AdmissionReplayConfig) -> Result<Self, ReplayConfigError> {
        config.validate()?;
        Ok(Self::from_validated(config))
    }

    fn from_validated(config: AdmissionReplayConfig) -> Self {
        Self {
            entries: RefCell::new(ReplayState::default()),
            config,
            capacity_denials: Cell::new(0),
            per_caller_denials: Cell::new(0),
            per_org_denials: Cell::new(0),
            external_pool_denials: Cell::new(0),
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

    /// Atomic insert-or-deny (the last step of §2.4).
    ///
    /// `now` is the monotonic clock in **milliseconds**, supplied by
    /// the caller (the guard never reads a clock); `expires_at` is
    /// `now + proof-remaining-ttl + retention skew` on the same
    /// monotonic timeline, precomputed by the caller. One borrow
    /// acquisition covers eviction, the collision check, and the
    /// insert.
    pub fn admit(
        &self,
        principal: ReplayPrincipal<'_>,
        call_id: u64,
        binding_digest: [u8; 32],
        expires_at: u64,
        now: u64,
    ) -> ReplayOutcome {
        let caller = principal.caller;
        let external = !principal.is_owner_org();
        let mut st = self.entries.borrow_mut();

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
                // Expired overwrite REUSES the occupied key, so
                // `total` and the per-caller count must not move.
                // The CHARGE must move anyway: the key is
                // `(caller, call_id)` and excludes `acting_org`, and
                // this branch runs only AFTER the window has expired —
                // the caller can return under a DIFFERENT verified
                // acting org, and a re-adopt can flip `external`.
                // `retarget` moves the replaced entry's counters onto
                // the entry actually stored; leaving them put strands
                // the old org's quota and makes `release` decrement
                // counters this entry never incremented.
                let replaced = inner.insert(
                    call_id,
                    ReplayEntry {
                        binding_digest,
                        expires_at,
                        acting_org: *principal.acting_org,
                        external,
                    },
                );
                if let Some(replaced) = replaced {
                    st.retarget(&replaced, external, principal.acting_org);
                }
                return ReplayOutcome::Admitted;
            }
        }

        // New key for this caller. Per-caller ceiling FIRST so a
        // flooding caller hits its own limit before it can pressure
        // the global cap. At capacity, reclaim only THIS caller's
        // expired slots; if still full, deny only this caller.
        let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
        if caller_live >= self.config.max_entries_per_caller {
            st.reclaim_caller(caller, now);
            let caller_live = st.by_caller.get(caller).map_or(0, HashMap::len);
            if caller_live >= self.config.max_entries_per_caller {
                self.per_caller_denials
                    .set(self.per_caller_denials.get() + 1);
                return ReplayOutcome::PerCallerCapacityExhausted;
            }
        }

        // The TRUST-DOMAIN quotas, checked before the global cap so
        // a flooding org exhausts its own allocation first and the
        // owner reserve is never reachable from outside. Owner-org
        // traffic is exempt from both: it is bounded per-identity and
        // by the global cap, and may borrow whatever external
        // capacity is idle. It cannot starve external callers — the
        // external pool is a floor for them in exactly the way the
        // reserve is a floor for the owner.
        if external {
            let org_live = st.org_live(principal.acting_org);
            if org_live >= self.config.max_entries_per_external_org {
                st.reclaim_all(now);
                if st.org_live(principal.acting_org) >= self.config.max_entries_per_external_org {
                    self.per_org_denials.set(self.per_org_denials.get() + 1);
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
                    self.external_pool_denials
                        .set(self.external_pool_denials.get() + 1);
                    return ReplayOutcome::ExternalPoolCapacityExhausted;
                }
            }
        }

        // Global ceiling. Reclaim EXPIRED slots fleet-wide; if none
        // are reclaimable, deny fail-closed rather than evict a live
        // guard. Still reachable for OWNER traffic (external traffic
        // is bounded tighter by the pool above).
        if st.total >= self.config.max_entries {
            st.reclaim_all(now);
            if st.total >= self.config.max_entries {
                self.capacity_denials.set(self.capacity_denials.get() + 1);
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

    /// Reclaim every entry whose window has closed as of `now` (u64
    /// mono ms). Optional maintenance — [`Self::admit`] reclaims
    /// lazily at capacity — but a periodic sweep keeps steady-state
    /// memory low. Returns how many entries were reclaimed.
    pub fn evict_expired(&self, now: u64) -> usize {
        self.entries.borrow_mut().reclaim_all(now)
    }

    /// Current tracked-entry count across all callers (test/metric
    /// surface).
    pub fn len(&self) -> usize {
        self.entries.borrow().total
    }

    /// `true` iff no entries are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().total == 0
    }

    /// Number of entries currently tracked for one caller
    /// (test/metric surface).
    pub fn caller_len(&self, caller: &EntityId) -> usize {
        self.entries
            .borrow()
            .by_caller
            .get(caller)
            .map_or(0, HashMap::len)
    }

    /// Total admissions denied for GLOBAL capacity since
    /// construction.
    pub fn capacity_denials(&self) -> u64 {
        self.capacity_denials.get()
    }

    /// Live entries charged to one acting org, across ALL of its
    /// member identities (test/metric surface).
    pub fn org_len(&self, org: &OrgId) -> usize {
        self.entries.borrow().org_live(org)
    }

    /// Live entries drawing on the EXTERNAL pool (test/metric
    /// surface).
    pub fn external_len(&self) -> usize {
        self.entries.borrow().external_total
    }

    /// Total admissions denied because one EXTERNAL ORG exhausted
    /// its aggregate allocation. A rising value here reads as "one
    /// grantee is misbehaving", distinct from
    /// [`Self::capacity_denials`] ("the whole guard is under
    /// pressure").
    pub fn per_org_denials(&self) -> u64 {
        self.per_org_denials.get()
    }

    /// Total admissions denied because the EXTERNAL POOL filled with
    /// no single org over quota — many active external orgs, owner
    /// org unaffected.
    pub fn external_pool_denials(&self) -> u64 {
        self.external_pool_denials.get()
    }

    /// Total admissions denied for PER-CALLER capacity since
    /// construction.
    pub fn per_caller_denials(&self) -> u64 {
        self.per_caller_denials.get()
    }
}

impl core::fmt::Debug for AdmissionReplayGuard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AdmissionReplayGuard")
            .field("config", &self.config)
            .field("len", &self.len())
            .field("capacity_denials", &self.capacity_denials.get())
            .field("per_caller_denials", &self.per_caller_denials.get())
            .field("per_org_denials", &self.per_org_denials.get())
            .field("external_pool_denials", &self.external_pool_denials.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(byte: u8) -> EntityId {
        EntityId::from_bytes([byte; 32])
    }

    /// The provider's OWN org.
    fn owner_org() -> OrgId {
        OrgId::from_bytes([0xAA; 32])
    }

    /// A distinct external org.
    fn external_org(byte: u8) -> OrgId {
        OrgId::from_bytes([byte; 32])
    }

    /// Admit as the provider's OWN org.
    fn admit_owner(
        guard: &AdmissionReplayGuard,
        caller: &EntityId,
        call_id: u64,
        digest: [u8; 32],
        expires: u64,
        now: u64,
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
        expires: u64,
        now: u64,
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

    /// A tiny envelope with the same SHAPE as the shipped default
    /// (reserve < total, per-org <= external pool).
    fn partitioned() -> AdmissionReplayGuard {
        AdmissionReplayGuard::new(AdmissionReplayConfig {
            max_entries: 40,
            owner_reserved_entries: 10, // external pool = 30
            max_entries_per_external_org: 8,
            max_entries_per_caller: 4,
        })
    }

    // ==================================================================
    // LEAF-2 — an expired overwrite must move the charge with the entry.
    //
    // The `(caller, call_id)` key EXCLUDES `acting_org`, and the overwrite
    // branch runs only AFTER the window has expired, so the same caller can
    // re-present the id verified for a DIFFERENT acting org — or flip    // `external` when the owner org changes under a re-adopt. The replaced
    // entry's counters must move with the stored entry, or its old org's
    // quota leaks upward until that org is denied with zero live entries,
    // and `release` later decrements counters the stored entry never
    // incremented — underflowing `external_total` on a flip (debug: panic
    // in the admission path; release: `usize::MAX`, every external caller
    // denied `ExternalPoolCapacityExhausted` forever).
    // ==================================================================

    /// An expired overwrite verified for a NEW acting org must free the
    /// replaced entry's quota from the old org and charge the new one.
    ///
    /// Pre-fix red: `assert_eq!(guard.org_len(&org_b), 1)` fails — the
    /// new org is never charged — and `assert_eq!(guard.org_len(&org_a),
    /// 0)` fails, stranding the old org's quota (that org is later
    /// denied `PerOrganizationCapacityExhausted` with zero live
    /// entries).
    #[test]
    fn an_expired_overwrite_frees_the_old_org_and_charges_the_new_one() {
        let guard = partitioned();
        let t0 = 1_000u64;
        let short = t0 + 5_000;
        let org_a = external_org(0xA1);
        let org_b = external_org(0xB1);

        assert_eq!(
            admit_external(&guard, &org_a, &caller(1), 7, [1u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );
        assert_eq!(guard.org_len(&org_a), 1);

        // The same key AFTER the window closes — now verified for org B.
        let later = t0 + 6_000;
        let new_expires = later + 5_000;
        assert_eq!(
            admit_external(&guard, &org_b, &caller(1), 7, [2u8; 32], new_expires, later),
            ReplayOutcome::Admitted,
        );

        assert_eq!(
            guard.org_len(&org_b),
            1,
            "the new org must be charged for the entry now stored under its name",
        );
        assert_eq!(
            guard.org_len(&org_a),
            0,
            "the replaced entry's quota must leave the old org, or the old org \
             is later denied with zero live entries",
        );
        assert_eq!(
            guard.len(),
            1,
            "the occupied key keeps its single global slot",
        );
        assert_eq!(guard.external_len(), 1);

        let last = later + 6_000;
        assert_eq!(guard.evict_expired(last), 1);
        assert_eq!(guard.len(), 0);
        assert_eq!(guard.org_len(&org_a), 0);
        assert_eq!(guard.org_len(&org_b), 0);
    }

    /// An expired overwrite whose `external` flag flips — the owner org
    /// can CHANGE under a re-adopt — must move the external-pool charge
    /// EXACTLY once in each direction.
    ///
    /// Pre-fix red: `assert_eq!(guard.external_len(), 1)` in the
    /// owner→external half fails (never charged) and
    /// `assert_eq!(guard.external_len(), 0)` in the external→owner half
    /// fails (never refunded) — the pool then bounds against numbers no
    /// live entry justifies.
    #[test]
    fn an_expired_overwrite_moves_the_external_pool_charge_exactly_once_each_way() {
        let t0 = 1_000u64;
        let short = t0 + 5_000;
        let later = t0 + 6_000;
        let new_expires = later + 5_000;
        let org_b = external_org(0xB1);

        // owner → external: external_total must RISE by exactly one.
        let guard = partitioned();
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, [1u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );
        assert_eq!(guard.external_len(), 0);
        assert_eq!(
            admit_external(&guard, &org_b, &caller(1), 7, [2u8; 32], new_expires, later),
            ReplayOutcome::Admitted,
        );
        assert_eq!(
            guard.external_len(),
            1,
            "an owner→external flip must add exactly one external slot",
        );
        assert_eq!(guard.org_len(&owner_org()), 0);
        assert_eq!(guard.org_len(&org_b), 1);

        // external → owner: external_total must FALL by exactly one.
        let guard = partitioned();
        assert_eq!(
            admit_external(&guard, &org_b, &caller(1), 7, [1u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );
        assert_eq!(guard.external_len(), 1);
        assert_eq!(
            admit_owner(&guard, &caller(1), 7, [2u8; 32], new_expires, later),
            ReplayOutcome::Admitted,
        );
        assert_eq!(
            guard.external_len(),
            0,
            "an external→owner flip must return exactly one external slot",
        );
        assert_eq!(guard.org_len(&org_b), 0);
        assert_eq!(guard.org_len(&owner_org()), 1);
    }

    /// After overwrites that moved charges (an org move and an
    /// `external` flip), releasing the live set must balance EVERY
    /// counter to zero — global, external pool, per-org, per-caller.
    ///
    /// Pre-fix red: `assert_eq!(guard.org_len(&org_a), 0)` fails with    /// the moved-from org still charged. Remove that assertion and the
    /// run instead dies inside `evict_expired` — the flipped entry's
    /// `release` underflows `external_total` ("attempt to subtract with
    /// overflow" in debug; `usize::MAX` in release, caught by
    /// `assert_eq!(guard.external_len(), 0)`).
    #[test]
    fn release_after_overwrites_balances_to_zero_across_the_live_set() {
        let guard = partitioned();
        let t0 = 1_000u64;
        let short = t0 + 5_000;
        let org_a = external_org(0xA1);
        let org_b = external_org(0xB1);
        let org_c = external_org(0xC1);

        // Three live entries.
        assert_eq!(
            admit_external(&guard, &org_a, &caller(1), 7, [1u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );
        assert_eq!(
            admit_owner(&guard, &caller(2), 8, [2u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );
        assert_eq!(
            admit_external(&guard, &org_c, &caller(3), 9, [3u8; 32], short, t0),
            ReplayOutcome::Admitted,
        );

        // Two expire and are overwritten with MOVED charges: an org
        // move (A→B) and an owner→external flip (owner→B). The third
        // entry is untouched.
        let later = t0 + 6_000;
        let new_expires = later + 5_000;
        assert_eq!(
            admit_external(&guard, &org_b, &caller(1), 7, [4u8; 32], new_expires, later),
            ReplayOutcome::Admitted,
        );
        assert_eq!(
            admit_external(&guard, &org_b, &caller(2), 8, [5u8; 32], new_expires, later),
            ReplayOutcome::Admitted,
        );
        assert_eq!(guard.len(), 3);
        assert_eq!(
            guard.org_len(&org_a),
            0,
            "the moved-from org's quota leaked upward",
        );

        let last = later + 6_000;
        assert_eq!(guard.evict_expired(last), 3);
        assert_eq!(guard.len(), 0, "global count leaked");
        assert_eq!(guard.external_len(), 0, "external-pool count leaked");
        assert_eq!(guard.org_len(&org_a), 0);
        assert_eq!(guard.org_len(&org_b), 0, "the moved-to org's quota leaked");
        assert_eq!(guard.org_len(&org_c), 0);
        assert_eq!(
            guard.org_len(&owner_org()),
            0,
            "the flipped-from owner quota leaked",
        );
        assert_eq!(guard.caller_len(&caller(1)), 0);
        assert_eq!(guard.caller_len(&caller(2)), 0);
        assert_eq!(guard.caller_len(&caller(3)), 0);
    }

    /// The underflow guard: `release` must never wrap `external_total`.
    /// Driven on the private state because the guarded drift is exactly
    /// the shape the overwrite bug used to manufacture — an entry marked
    /// `external` released while `external_total` is already zero — and
    /// no public call sequence reaches it once the charge moves with the
    /// stored entry.
    ///
    /// Pre-fix red: `st.release` panics ("attempt to subtract with
    /// overflow" in debug); in a release build the wrap to `usize::MAX`
    /// is caught by `assert_eq!(st.external_total, 0)`.
    #[test]
    fn releasing_an_external_entry_with_zero_live_external_entries_saturates() {
        let owner = owner_org();
        let mut st = ReplayState::default();
        // One live OWNER entry — so `total` is sound but ZERO entries
        // draw on the external pool.
        st.charge(false, &owner);
        let stranded = ReplayEntry {
            binding_digest: [7u8; 32],
            expires_at: 0,
            acting_org: external_org(0xB1),
            external: true,
        };
        st.release(&stranded);
        assert_eq!(
            st.external_total, 0,
            "external_total wrapped instead of saturating",
        );
        assert_eq!(st.total, 0);
    }
}
