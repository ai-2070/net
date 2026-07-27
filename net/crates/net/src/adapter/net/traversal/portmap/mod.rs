//! Port-mapping orchestration — UPnP-IGD + NAT-PMP / PCP clients
//! behind a common trait, plus the task that installs, renews,
//! and revokes mappings on the operator's router.
//!
//! **Framing.** Port mapping is a **latency / throughput
//! optimization**, not a correctness requirement. A mesh node
//! whose router doesn't speak UPnP / NAT-PMP / PCP still reaches
//! every peer through the routed-handshake path. A failed probe,
//! refused install, or renewal timeout just means the mesh stayed
//! on the relay.
//!
//! # Shape
//!
//! - [`PortMapperClient`] — async trait every protocol implements.
//!   Stage 4b-1 ships [`NullPortMapper`] (always `Unavailable`)
//!   and [`MockPortMapperClient`] (programmable response queue
//!   for unit tests). Stages 4b-2 / 4b-3 land `NatPmpMapper` +
//!   `UpnpMapper`; stage 4b-4 adds a sequencer.
//! - [`PortMapping`] — the return value from a successful
//!   install, carrying the external `SocketAddr` + TTL + which
//!   protocol produced it.
//! - [`PortMappingError`] — typed failures (unavailable, timeout,
//!   transport, refused).
//! - [`PortMapperTask`] — the state machine that wraps a client
//!   and drives install / renew / revoke against a
//!   [`MappingSink`].
//! - [`MappingSink`] — the thin handle the task uses to push
//!   installed mappings into `MeshNode`'s runtime override +
//!   stats atomics. Kept trait-free so the task is testable
//!   without spinning up a full `MeshNode`.
//!
//! # Lifecycle
//!
//! See `docs/internal/plans/PORT_MAPPING_PLAN.md` decision 3 for the state
//! machine diagram. One-shot install at spawn; renewal every
//! [`super::TraversalConfig::port_mapping_renewal`]; three
//! consecutive renewal failures revoke; shutdown revokes too.

pub mod gateway;
pub mod natpmp;
pub mod sequential;
pub mod upnp;

pub use sequential::{sequential_mapper_from_os, SequentialMapper};

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use tokio::sync::Notify;

use super::classify::NatClass;
use super::TraversalStats;

// =========================================================================
// Wire types
// =========================================================================

/// Which protocol produced a given port mapping. Stable wire
/// vocabulary — a caller that inspects `PortMapping.protocol`
/// (e.g., for operator dashboards) sees one of these two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// NAT-PMP (RFC 6886) or PCP (RFC 6887). Shared wire format.
    NatPmp,
    /// UPnP Internet Gateway Device control protocol (IGD v1/v2).
    Upnp,
}

impl Protocol {
    /// Stable string form for stats / dashboards. Never
    /// localized; matches the `"nat-pmp" | "upnp"` vocabulary
    /// the binding surfaces emit in stage 4b-5 telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::NatPmp => "nat-pmp",
            Protocol::Upnp => "upnp",
        }
    }
}

/// A live port mapping installed on the operator's router. The
/// `external` field is what the mesh advertises via
/// `set_reflex_override`; `ttl` is the router's declared lease
/// length (renewal must beat it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMapping {
    /// The public `SocketAddr` the router assigned — what
    /// remote peers see as this node's reachable address.
    pub external: std::net::SocketAddr,
    /// The internal UDP port we asked the router to map to.
    /// Usually equal to the mesh's bind port.
    pub internal_port: u16,
    /// Router-declared lease length. Renewal cadence must be
    /// shorter; the task re-issues on
    /// [`super::TraversalConfig::port_mapping_renewal`] regardless
    /// of TTL to absorb clock skew + flaky networks.
    pub ttl: Duration,
    /// Which protocol produced this mapping. Locked in at
    /// install time — a given task doesn't mix protocols.
    pub protocol: Protocol,
}

/// Typed failures from a [`PortMapperClient`] call. Each
/// variant maps to a stable `kind` string on the SDK binding
/// surface (stage 4b-5): `"unavailable" | "timeout" |
/// "transport" | "refused"`.
#[derive(Debug, thiserror::Error, Clone)]
pub enum PortMappingError {
    /// Neither NAT-PMP nor UPnP responded within its probe
    /// deadline, or the router explicitly reported no mapping
    /// capability. Most common outcome on networks where the
    /// operator has turned UPnP off or the router doesn't
    /// support either protocol.
    #[error("unavailable")]
    Unavailable,

    /// A single call (probe / install / renew / remove) took
    /// longer than the per-protocol deadline (1 s NAT-PMP,
    /// 2 s UPnP). The task treats this the same as
    /// `Unavailable` for probe; for renew it contributes to
    /// the 3-strike revoke count.
    #[error("timeout")]
    Timeout,

    /// Socket-level error talking to the router.
    #[error("transport: {0}")]
    Transport(String),

    /// The router responded but refused the request (e.g.
    /// NAT-PMP `NOT_AUTHORIZED`, UPnP `ConflictInMappingEntry`).
    #[error("refused: {0}")]
    Refused(String),
}

impl PortMappingError {
    /// Stable machine-readable kind string. Never localized;
    /// stage 4b-5 binding surfaces use this for `errors.Is`-style
    /// checks without parsing message text.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Transport(_) => "transport",
            Self::Refused(_) => "refused",
        }
    }
}

// =========================================================================
// Trait
// =========================================================================

/// An async client for a single port-mapping protocol
/// (NAT-PMP / PCP or UPnP-IGD). The [`PortMapperTask`] holds a
/// `Box<dyn PortMapperClient>` and drives it through
/// probe → install → renew → remove.
///
/// Stage 4b-1 ships two implementations:
///
/// - [`NullPortMapper`] — always returns `Unavailable`. Default
///   when `try_port_mapping(true)` is set but no real client
///   has been registered (i.e., before stage 4b-2 / 4b-3 land
///   real UPnP + NAT-PMP impls).
/// - [`MockPortMapperClient`] — programmable response queue
///   for unit tests. Drives the [`PortMapperTask`] lifecycle
///   deterministically.
#[async_trait]
pub trait PortMapperClient: Send + Sync {
    /// Short-deadline reachability check. Returns `Ok(())` if
    /// the protocol is available on the gateway, an error
    /// otherwise. Called once per task spawn before the first
    /// `install`.
    async fn probe(&self) -> Result<(), PortMappingError>;

    /// Request a mapping for `internal_port` with the given
    /// TTL. Returns the external `SocketAddr` the router
    /// assigned. Called once per task spawn after a successful
    /// probe.
    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError>;

    /// Refresh an existing mapping. Most protocols re-issue
    /// the install request as a refresh. Called on every
    /// `TraversalConfig::port_mapping_renewal` tick.
    async fn renew(&self, mapping: &PortMapping) -> Result<PortMapping, PortMappingError>;

    /// Ask the router to drop the mapping. Best-effort —
    /// failures are logged + ignored. Called on task shutdown
    /// or after a revoke transition.
    async fn remove(&self, mapping: &PortMapping);
}

// =========================================================================
// MappingSink — the task's write surface
// =========================================================================

/// Handle to the subset of `MeshNode` state a
/// [`PortMapperTask`] needs to write. Kept trait-free
/// (concrete struct) so tests can exercise the task without
/// constructing a full `MeshNode`.
///
/// Owns `Arc`-clones of the atomics the task updates:
///
/// - Port-mapping stats (`port_mapping_active`,
///   `port_mapping_external`, `port_mapping_renewals`) on
///   [`TraversalStats`].
/// - The reflex-override trio (`reflex_addr`, `nat_class`,
///   `reflex_override_active`) on `MeshNode` — the same three
///   fields `set_reflex_override` / `clear_reflex_override`
///   mutate.
///
/// Installing a mapping atomically pins the reflex to the
/// mapped external address (same semantics as the stage-4a
/// runtime setter); revoking clears it.
#[derive(Clone)]
pub struct MappingSink {
    stats: Arc<TraversalStats>,
    reflex_addr: Arc<ArcSwapOption<std::net::SocketAddr>>,
    nat_class: Arc<AtomicU8>,
    reflex_override_active: Arc<AtomicBool>,
    /// Shared publication mutex — the same one `MeshNode` holds
    /// around its triple-atomic writes + multi-field reads.
    /// Held briefly across the sink's `apply_install` /
    /// `apply_revoke` / `apply_renewal` critical sections so a
    /// concurrent `announce_capabilities_with` can't observe a
    /// torn state across the port-mapping task's updates
    /// (cubic P1). The mutex is unit type — the data it's
    /// guarding is the set of atomics above; the lock only
    /// provides a publication barrier.
    publish_mu: Arc<parking_lot::Mutex<()>>,
}

impl MappingSink {
    /// Construct a sink from the three reflex-override atomics
    /// `MeshNode` already owns, plus the stats block and the
    /// shared publication mutex. Called from
    /// `MeshNode::spawn_port_mapping_loop` where all five are
    /// available as `Arc`-clones.
    pub fn new(
        stats: Arc<TraversalStats>,
        reflex_addr: Arc<ArcSwapOption<std::net::SocketAddr>>,
        nat_class: Arc<AtomicU8>,
        reflex_override_active: Arc<AtomicBool>,
        publish_mu: Arc<parking_lot::Mutex<()>>,
    ) -> Self {
        Self {
            stats,
            reflex_addr,
            nat_class,
            reflex_override_active,
            publish_mu,
        }
    }

    /// Apply a freshly-installed mapping: flip the stats, stamp
    /// the reflex-override atomics to `Open + external`. Same
    /// publish order as `MeshNode::set_reflex_override` (reflex
    /// and class first, flag last) so a concurrent announce
    /// either sees the pre-install state or the fully-installed
    /// override, never a torn mix.
    pub(crate) fn apply_install(&self, mapping: &PortMapping) {
        // Stats first — observability paths reading the stats
        // block see the external address match up with the
        // reflex_addr below.
        self.stats.record_port_mapping_install(mapping.external);

        // Reflex override (matches `MeshNode::set_reflex_override`),
        // with the same publication mutex held across the triple.
        let _g = self.publish_mu.lock();
        self.reflex_addr.store(Some(Arc::new(mapping.external)));
        self.nat_class
            .store(NatClass::Open.as_u8(), Ordering::Release);
        self.reflex_override_active.store(true, Ordering::Release);
    }

    /// Record a successful renewal. If `mapping.external`
    /// matches the currently-advertised reflex, this is a pure
    /// stats bump (renewal counter only). If it *differs* —
    /// router reboot, WAN interface flap, DHCP lease change —
    /// the new external has to be re-published on both the
    /// reflex-override atomic (so capability announcements
    /// carry the fresh address) and the stats block (so
    /// observability sees the current mapping). The override
    /// flag + NAT class stay set; only the address field
    /// moves. Without this propagation the renewal loop silently
    /// advertises a dead address to peers until the next install
    /// / revoke cycle.
    pub(crate) fn apply_renewal(&self, mapping: &PortMapping) {
        let prev = self.reflex_addr.load_full().map(|arc| *arc);
        if prev != Some(mapping.external) {
            // Publish order mirrors `apply_install` (reflex
            // first, stats second) so a concurrent announce
            // either sees the old address or the new one, never
            // a torn mix. No need to re-flip the override flag
            // — it's already set and stays set across the
            // rebind. Mutex held across the publish step so
            // `announce_capabilities_with` doesn't observe a
            // partial snapshot during the rebind.
            let _g = self.publish_mu.lock();
            self.reflex_addr.store(Some(Arc::new(mapping.external)));
            self.stats.replace_port_mapping_external(mapping.external);
        }
        self.stats.record_port_mapping_renewal();
    }

    /// Revoke the current mapping: flip stats to inactive, drop
    /// the reflex override (matches `MeshNode::clear_reflex_override`).
    pub(crate) fn apply_revoke(&self) {
        self.stats.record_port_mapping_revoke();
        // clear_reflex_override semantics with the shared
        // publication mutex held across the triple.
        let _g = self.publish_mu.lock();
        if self.reflex_override_active.swap(false, Ordering::AcqRel) {
            self.reflex_addr.store(None);
            self.nat_class
                .store(NatClass::Unknown.as_u8(), Ordering::Release);
        }
    }
}

// =========================================================================
// NullPortMapper
// =========================================================================

/// A [`PortMapperClient`] that reports the protocol as
/// unavailable on every call. Used by `MeshNode::start` as the
/// default when `try_port_mapping(true)` is set but no real
/// client has been registered — the task immediately transitions
/// to `Exited`, the classifier sweeps as usual, and there's no
/// operator-visible hang or error.
#[derive(Debug, Default)]
pub struct NullPortMapper;

impl NullPortMapper {
    /// Construct a new null mapper. Identical to
    /// `Default::default()` — kept as a named constructor so
    /// call sites read consistently against the other clients.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PortMapperClient for NullPortMapper {
    async fn probe(&self) -> Result<(), PortMappingError> {
        Err(PortMappingError::Unavailable)
    }

    async fn install(
        &self,
        _internal_port: u16,
        _ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        Err(PortMappingError::Unavailable)
    }

    async fn renew(&self, _mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        Err(PortMappingError::Unavailable)
    }

    async fn remove(&self, _mapping: &PortMapping) {
        // No-op; null mapper has nothing on the router to revoke.
    }
}

// =========================================================================
// MockPortMapperClient — unit-test driver
// =========================================================================

/// A programmable [`PortMapperClient`] for unit-testing the
/// [`PortMapperTask`] lifecycle. Each verb has its own response
/// queue; calls pop from the front.
///
/// Queue semantics:
///
/// - An empty queue returns `Err(Unavailable)` — the test hasn't
///   programmed a response, so fail deterministically.
/// - `remove` doesn't return a Result, so its "queue" is just a
///   counter tracking how many times it was called.
///
/// Thread-safe: all fields use `Mutex` / atomics so the task's
/// background thread can call into it without cloning the mock.
#[derive(Debug)]
pub struct MockPortMapperClient {
    probe_results: parking_lot::Mutex<std::collections::VecDeque<Result<(), PortMappingError>>>,
    install_results:
        parking_lot::Mutex<std::collections::VecDeque<Result<PortMapping, PortMappingError>>>,
    renew_results:
        parking_lot::Mutex<std::collections::VecDeque<Result<PortMapping, PortMappingError>>>,
    remove_calls: std::sync::atomic::AtomicU32,
}

impl MockPortMapperClient {
    /// Create an empty mock. Tests queue responses via the
    /// `queue_*` methods before spawning the task.
    pub fn new() -> Self {
        Self {
            probe_results: parking_lot::Mutex::new(Default::default()),
            install_results: parking_lot::Mutex::new(Default::default()),
            renew_results: parking_lot::Mutex::new(Default::default()),
            remove_calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Queue a response for the next `probe()` call.
    pub fn queue_probe(&self, result: Result<(), PortMappingError>) {
        self.probe_results.lock().push_back(result);
    }

    /// Queue a response for the next `install()` call.
    pub fn queue_install(&self, result: Result<PortMapping, PortMappingError>) {
        self.install_results.lock().push_back(result);
    }

    /// Queue a response for the next `renew()` call.
    pub fn queue_renew(&self, result: Result<PortMapping, PortMappingError>) {
        self.renew_results.lock().push_back(result);
    }

    /// How many times `remove()` has been called. Monotonic.
    pub fn remove_call_count(&self) -> u32 {
        self.remove_calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of queued `probe()` responses not yet consumed.
    /// Tests use this to assert a code path either DID or DID
    /// NOT call `probe()` — e.g. the sequential-mapper fallback
    /// must probe NAT-PMP before installing, so a regression
    /// leaves an unconsumed probe entry behind.
    pub fn remaining_probes(&self) -> usize {
        self.probe_results.lock().len()
    }

    /// Number of queued `install()` responses not yet consumed.
    /// Mirror of [`Self::remaining_probes`] — lets a test assert
    /// that a code path short-circuited before calling install.
    pub fn remaining_installs(&self) -> usize {
        self.install_results.lock().len()
    }
}

impl Default for MockPortMapperClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PortMapperClient for MockPortMapperClient {
    async fn probe(&self) -> Result<(), PortMappingError> {
        self.probe_results
            .lock()
            .pop_front()
            .unwrap_or(Err(PortMappingError::Unavailable))
    }

    async fn install(
        &self,
        _internal_port: u16,
        _ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        self.install_results
            .lock()
            .pop_front()
            .unwrap_or(Err(PortMappingError::Unavailable))
    }

    async fn renew(&self, _mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        self.renew_results
            .lock()
            .pop_front()
            .unwrap_or(Err(PortMappingError::Unavailable))
    }

    async fn remove(&self, _mapping: &PortMapping) {
        self.remove_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// =========================================================================
// PortMapperTask — the state machine
// =========================================================================

/// Default lease length to request from the router. Most
/// consumer gateways accept 3600 s and renew on re-request;
/// we re-issue on `TraversalConfig::port_mapping_renewal`
/// (default 30 min) which is well inside the TTL.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// After this many consecutive renewal failures, the task
/// revokes the mapping and exits. Matches the
/// `FailureDetector::miss_threshold` value used elsewhere in
/// the mesh for symmetric operator intuition.
pub(crate) const RENEWAL_FAILURE_THRESHOLD: u32 = 3;

/// Drives the port-mapping lifecycle for a single mesh node.
/// See module docs + `docs/internal/plans/PORT_MAPPING_PLAN.md` decision 3.
///
/// Tests construct this directly and drive it against a
/// [`MockPortMapperClient`]; production callers go through
/// `MeshNode::spawn_port_mapping_loop`.
pub struct PortMapperTask {
    client: Box<dyn PortMapperClient>,
    sink: MappingSink,
    internal_port: u16,
    renewal_interval: Duration,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
}

impl PortMapperTask {
    /// Construct the task. Doesn't spawn; call [`Self::run`]
    /// inside a `tokio::spawn` to drive the lifecycle.
    pub fn new(
        client: Box<dyn PortMapperClient>,
        sink: MappingSink,
        internal_port: u16,
        renewal_interval: Duration,
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
    ) -> Self {
        Self {
            client,
            sink,
            internal_port,
            renewal_interval,
            shutdown,
            shutdown_notify,
        }
    }

    /// Drive the state machine to completion. Returns when
    /// shutdown fires or the mapping is revoked (after 3
    /// consecutive renewal failures, or from an initial-install
    /// failure).
    pub async fn run(self) {
        // Step 1: probe. A probe failure ends the task without
        // any override side-effects — the classifier runs as
        // usual. No retry (plan decision 3: best-effort side
        // quest).
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        if self.client.probe().await.is_err() {
            return;
        }

        // Step 2: install. Same one-shot contract.
        let mut mapping = match self.client.install(self.internal_port, DEFAULT_TTL).await {
            Ok(m) => m,
            Err(_) => return,
        };

        self.sink.apply_install(&mapping);

        // Step 3: renewal loop.
        //
        // Respect the granted lease TTL — a gateway that grants 60s
        // leases should be renewed on a ~30s cadence, not the
        // configured 30-minute default. We pick
        // `min(renewal_interval, mapping.ttl/2)` as the effective
        // tick so a short-lease gateway doesn't leave the mesh
        // advertising a dead address for ~29 of every 30 minutes.
        //
        // On a transient renewal failure we wait a short
        // `RETRY_BACKOFF` (200ms) and try again before counting it
        // against `RENEWAL_FAILURE_THRESHOLD`. Previously a single
        // hiccup ate the full ticker interval before the next attempt
        // — with a 60s lease and 30min ticker that's 90+ minutes of
        // dead-address advertisement before revoke fires.
        const RETRY_BACKOFF: Duration = Duration::from_millis(200);

        // Free helper rather than a closure so it doesn't capture
        // `mapping` (which we mutate inside the loop). Borrowing
        // a closure across the `mapping = next;` reassignment
        // would deny the assignment under the borrow checker.
        fn effective_interval(renewal_interval: Duration, ttl: Duration) -> Duration {
            let configured = if renewal_interval.is_zero() {
                Duration::from_secs(1)
            } else {
                renewal_interval
            };
            let half_ttl = ttl / 2;
            if !half_ttl.is_zero() && half_ttl < configured {
                half_ttl
            } else {
                configured
            }
        }
        let mut ticker =
            tokio::time::interval(effective_interval(self.renewal_interval, mapping.ttl));
        // First tick is immediate; skip — we just installed.
        ticker.tick().await;

        let mut consecutive_failures: u32 = 0;

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                _ = self.shutdown_notify.notified() => break,
                _ = ticker.tick() => {
                    let outcome = match self.client.renew(&mapping).await {
                        Ok(next) => Ok(next),
                        Err(e) => {
                            // #22: short-retry once before giving up.
                            tokio::time::sleep(RETRY_BACKOFF).await;
                            if self.shutdown.load(Ordering::Acquire) {
                                break;
                            }
                            tracing::debug!(
                                error = %e,
                                "PortMapper renew: transient failure, retrying after {:?}",
                                RETRY_BACKOFF
                            );
                            self.client.renew(&mapping).await
                        }
                    };
                    match outcome {
                        Ok(next) => {
                            consecutive_failures = 0;
                            self.sink.apply_renewal(&next);
                            mapping = next;
                            // Re-derive ticker if the new lease's
                            // TTL changed our effective cadence.
                            let new_interval =
                                effective_interval(self.renewal_interval, mapping.ttl);
                            if new_interval != ticker.period() {
                                ticker = tokio::time::interval(new_interval);
                                ticker.tick().await; // immediate first tick
                            }
                        }
                        Err(_) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            if consecutive_failures >= RENEWAL_FAILURE_THRESHOLD {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Revoke phase: remove on the router (best-effort), then
        // clear the override + stats. Matches parent decision 12
        // — if the remove call fails / times out, the router
        // cleans up on TTL expiry.
        self.client.remove(&mapping).await;
        self.sink.apply_revoke();
    }
}

/// Blanket impl so `Arc<C>` delegates to the inner client. Lets
/// unit tests hold both the mock (to queue responses) and hand
/// the task a `Box<dyn PortMapperClient>` wrapping the same
/// inner value — without this, the mock would need an explicit
/// split between control-side + task-side handles.
#[async_trait]
impl<C> PortMapperClient for Arc<C>
where
    C: PortMapperClient + ?Sized,
{
    async fn probe(&self) -> Result<(), PortMappingError> {
        C::probe(self).await
    }
    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        C::install(self, internal_port, ttl).await
    }
    async fn renew(&self, mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        C::renew(self, mapping).await
    }
    async fn remove(&self, mapping: &PortMapping) {
        C::remove(self, mapping).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn sample_sink() -> (MappingSink, Arc<TraversalStats>, Arc<AtomicBool>) {
        let stats = Arc::new(TraversalStats::new());
        let reflex_addr = Arc::new(ArcSwapOption::empty());
        let nat_class = Arc::new(AtomicU8::new(NatClass::Unknown.as_u8()));
        let override_active = Arc::new(AtomicBool::new(false));
        let publish_mu = Arc::new(parking_lot::Mutex::new(()));
        let sink = MappingSink::new(
            Arc::clone(&stats),
            reflex_addr,
            nat_class,
            Arc::clone(&override_active),
            publish_mu,
        );
        (sink, stats, override_active)
    }

    fn sample_mapping() -> PortMapping {
        PortMapping {
            external: "203.0.113.42:9001".parse().unwrap(),
            internal_port: 9001,
            ttl: Duration::from_secs(3600),
            protocol: Protocol::NatPmp,
        }
    }

    #[test]
    fn null_port_mapper_always_unavailable() {
        let mapper = NullPortMapper::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(matches!(
                mapper.probe().await,
                Err(PortMappingError::Unavailable)
            ));
            assert!(matches!(
                mapper.install(9001, DEFAULT_TTL).await,
                Err(PortMappingError::Unavailable)
            ));
            let m = sample_mapping();
            assert!(matches!(
                mapper.renew(&m).await,
                Err(PortMappingError::Unavailable)
            ));
            mapper.remove(&m).await;
        });
    }

    #[test]
    fn port_mapping_error_kind_stable() {
        // Cross-binding parity: each variant maps to the stable
        // string the plan decision 1 names.
        assert_eq!(PortMappingError::Unavailable.kind(), "unavailable");
        assert_eq!(PortMappingError::Timeout.kind(), "timeout");
        assert_eq!(
            PortMappingError::Transport("socket died".into()).kind(),
            "transport"
        );
        assert_eq!(
            PortMappingError::Refused("NOT_AUTHORIZED".into()).kind(),
            "refused"
        );
    }

    #[test]
    fn protocol_as_str_stable() {
        assert_eq!(Protocol::NatPmp.as_str(), "nat-pmp");
        assert_eq!(Protocol::Upnp.as_str(), "upnp");
    }

    #[test]
    fn mapping_sink_apply_install_flips_override() {
        let (sink, stats, override_active) = sample_sink();
        let mapping = sample_mapping();

        sink.apply_install(&mapping);

        assert!(override_active.load(Ordering::Acquire));
        let snap = stats.snapshot();
        assert!(snap.port_mapping_active);
        assert_eq!(snap.port_mapping_external, Some(mapping.external));
        assert_eq!(snap.port_mapping_renewals, 0);
    }

    #[test]
    fn mapping_sink_apply_renewal_bumps_counter() {
        let (sink, stats, _) = sample_sink();
        let mapping = sample_mapping();
        sink.apply_install(&mapping);
        assert_eq!(stats.snapshot().port_mapping_renewals, 0);

        // Same external across ticks — the common path. Address
        // doesn't change, renewal counter bumps, reflex/stats
        // stay put.
        sink.apply_renewal(&mapping);
        sink.apply_renewal(&mapping);
        sink.apply_renewal(&mapping);

        assert_eq!(stats.snapshot().port_mapping_renewals, 3);
        assert_eq!(
            stats.snapshot().port_mapping_external,
            Some(mapping.external)
        );
    }

    /// Regression for a cubic-flagged P2 bug: on successful
    /// renewal the task was overwriting the local `mapping`
    /// variable but `apply_renewal` (counter-only) was leaving
    /// the `reflex_addr` and `port_mapping_external` stats
    /// stale. If the router returned a different external on
    /// renewal (reboot, WAN flap, DHCP churn), the mesh kept
    /// advertising the dead address to peers until the next
    /// install / revoke cycle.
    ///
    /// With the fix, `apply_renewal` takes `&PortMapping` and
    /// re-publishes the external if it changed. This test pins
    /// that path.
    #[test]
    fn mapping_sink_apply_renewal_propagates_new_external() {
        use crate::adapter::net::traversal::classify::NatClass;

        let (sink, stats, override_active) = sample_sink();
        let initial = sample_mapping();
        let initial_external = initial.external;
        sink.apply_install(&initial);
        assert_eq!(
            stats.snapshot().port_mapping_external,
            Some(initial_external)
        );

        // Router returns a new external address on the next
        // renewal tick. Simulate a WAN-rebind scenario.
        let new_external: std::net::SocketAddr = "203.0.113.200:55555".parse().unwrap();
        assert_ne!(new_external, initial_external, "test precondition");
        let rebind = PortMapping {
            external: new_external,
            internal_port: initial.internal_port,
            ttl: initial.ttl,
            protocol: initial.protocol,
        };

        sink.apply_renewal(&rebind);

        let snap = stats.snapshot();
        assert_eq!(
            snap.port_mapping_external,
            Some(new_external),
            "stats must carry the new mapped external, not the stale one",
        );
        assert_eq!(
            snap.port_mapping_renewals, 1,
            "renewal counter should still bump",
        );
        assert!(
            override_active.load(Ordering::Acquire),
            "override flag stays set across a rebind — only the address moves",
        );
        // nat_class stays Open — no demotion on address change.
        let nc = sink.nat_class.load(Ordering::Acquire);
        assert_eq!(NatClass::from_u8(nc), NatClass::Open);
    }

    /// Complement of the above: when the router returns the same
    /// external on renewal (the common case), the reflex atomic
    /// isn't re-stored. ArcSwap writes are cheap but not free,
    /// and we want the fast path to stay fast.
    #[test]
    fn mapping_sink_apply_renewal_skips_republish_when_external_unchanged() {
        let (sink, stats, _) = sample_sink();
        let mapping = sample_mapping();
        sink.apply_install(&mapping);

        // Take a snapshot of the reflex pointer identity after
        // install. Without a rebind, apply_renewal must not
        // replace the Arc — repeated renewals shouldn't churn
        // ArcSwap's hazard-pointer bookkeeping.
        let before = sink.reflex_addr.load_full().unwrap();
        sink.apply_renewal(&mapping);
        sink.apply_renewal(&mapping);
        let after = sink.reflex_addr.load_full().unwrap();

        assert!(
            Arc::ptr_eq(&before, &after),
            "unchanged external must not re-store the reflex Arc; got a new pointer",
        );
        assert_eq!(stats.snapshot().port_mapping_renewals, 2);
    }

    #[test]
    fn mapping_sink_apply_revoke_clears_override() {
        let (sink, stats, override_active) = sample_sink();
        sink.apply_install(&sample_mapping());
        assert!(override_active.load(Ordering::Acquire));

        sink.apply_revoke();
        assert!(!override_active.load(Ordering::Acquire));
        let snap = stats.snapshot();
        assert!(!snap.port_mapping_active);
        assert_eq!(snap.port_mapping_external, None);
    }

    #[test]
    fn mapping_sink_revoke_is_idempotent() {
        let (sink, _, override_active) = sample_sink();
        // No install — revoke on fresh state must not set the
        // override true, and must not panic.
        sink.apply_revoke();
        assert!(!override_active.load(Ordering::Acquire));

        // Install then double-revoke — second is a no-op.
        sink.apply_install(&sample_mapping());
        sink.apply_revoke();
        sink.apply_revoke();
        assert!(!override_active.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_exits_on_probe_failure_without_side_effects() {
        // Probe returns Unavailable → task exits immediately
        // without touching the override atomics or stats.
        let (sink, stats, override_active) = sample_sink();
        let mock = Arc::new(MockPortMapperClient::new());
        // Default empty queue → probe returns Unavailable.
        let task = PortMapperTask::new(
            Box::new(Arc::clone(&mock)),
            sink,
            9001,
            Duration::from_millis(50),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
        );
        task.run().await;

        assert!(!override_active.load(Ordering::Acquire));
        assert!(!stats.snapshot().port_mapping_active);
        assert_eq!(mock.remove_call_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_installs_on_probe_and_install_success() {
        let (sink, stats, override_active) = sample_sink();
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Ok(()));
        mock.queue_install(Ok(sample_mapping()));
        // No renew queued — every tick fails; after 3 ticks the
        // task revokes + exits. Use a short interval to make the
        // test run in well under a second.
        let shutdown = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let task = PortMapperTask::new(
            Box::new(Arc::clone(&mock)),
            sink,
            9001,
            Duration::from_millis(20),
            Arc::clone(&shutdown),
            Arc::clone(&notify),
        );
        let handle = tokio::spawn(task.run());

        // Wait for the install to land.
        let mut installed = false;
        for _ in 0..50 {
            if stats.snapshot().port_mapping_active {
                installed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(installed, "install should have fired");
        assert!(override_active.load(Ordering::Acquire));
        assert_eq!(
            stats.snapshot().port_mapping_external,
            Some(sample_mapping().external),
        );

        // Wait for 3-strike revoke. 3 ticks at 20 ms = ~60 ms
        // + scheduler jitter; cap at 1 s.
        let task_done = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(
            task_done.is_ok(),
            "task should exit after 3 renewal failures"
        );

        // Post-revoke state: override cleared, remove() called.
        assert!(!override_active.load(Ordering::Acquire));
        assert!(!stats.snapshot().port_mapping_active);
        assert_eq!(mock.remove_call_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_survives_transient_renewal_failure() {
        // 2 consecutive failures then success — task stays
        // running, renewal counter eventually bumps.
        let (sink, stats, override_active) = sample_sink();
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Ok(()));
        mock.queue_install(Ok(sample_mapping()));
        mock.queue_renew(Err(PortMappingError::Timeout));
        mock.queue_renew(Err(PortMappingError::Timeout));
        mock.queue_renew(Ok(sample_mapping()));
        // After the Ok renewal, the queue is empty again — the
        // following 3 ticks all return Unavailable, triggering
        // revoke. That's fine; we just need to observe that we
        // survived 2 failures without revoking first.
        let shutdown = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let task = PortMapperTask::new(
            Box::new(Arc::clone(&mock)),
            sink,
            9001,
            Duration::from_millis(20),
            Arc::clone(&shutdown),
            Arc::clone(&notify),
        );
        let handle = tokio::spawn(task.run());

        // Wait for the renewal counter to hit 1 — that only
        // happens after the 2-failure-then-success sequence.
        let mut renewed = false;
        for _ in 0..200 {
            if stats.snapshot().port_mapping_renewals >= 1 {
                renewed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            renewed,
            "renewal counter should bump after 2 transient failures → success",
        );
        // Override should still be active during the renew window.
        assert!(override_active.load(Ordering::Acquire));

        // Let the task exit eventually so we don't leak the
        // spawn; we don't care about the exact exit condition.
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_exits_cleanly_on_shutdown() {
        let (sink, stats, override_active) = sample_sink();
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Ok(()));
        mock.queue_install(Ok(sample_mapping()));
        // Queue endless renewal successes so only shutdown can
        // end the task.
        for _ in 0..100 {
            mock.queue_renew(Ok(sample_mapping()));
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let task = PortMapperTask::new(
            Box::new(Arc::clone(&mock)),
            sink,
            9001,
            Duration::from_millis(30),
            Arc::clone(&shutdown),
            Arc::clone(&notify),
        );
        let handle = tokio::spawn(task.run());

        // Wait for install.
        let mut installed = false;
        for _ in 0..50 {
            if stats.snapshot().port_mapping_active {
                installed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(installed);

        // Fire shutdown.
        shutdown.store(true, Ordering::Release);
        notify.notify_waiters();

        let exit = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(exit.is_ok(), "task should exit within 1 s of shutdown");

        // Post-shutdown: remove called once, override cleared,
        // stats reflect revoked state.
        assert_eq!(mock.remove_call_count(), 1);
        assert!(!override_active.load(Ordering::Acquire));
        let snap = stats.snapshot();
        assert!(!snap.port_mapping_active);
        assert_eq!(snap.port_mapping_external, None);
    }

    #[test]
    fn stats_snapshot_includes_port_mapping_fields() {
        // Guard: a caller doing `stats.snapshot()` on a
        // zero-initialized block sees `port_mapping_active =
        // false` and no external address. Renewals counter
        // starts at zero.
        let stats = Arc::new(TraversalStats::new());
        let _renewals_to_suppress_warning: AtomicU64 = AtomicU64::new(0);
        let snap = stats.snapshot();
        assert!(!snap.port_mapping_active);
        assert_eq!(snap.port_mapping_external, None);
        assert_eq!(snap.port_mapping_renewals, 0);
    }
}
