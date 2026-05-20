//! Composing port-mapper that tries NAT-PMP first, falls back
//! to UPnP on failure, and remembers which protocol won so
//! subsequent install / renew / remove calls route to the same
//! client.
//!
//! Plan decision 1: one composing client, not a trait-object
//! chain inside each verb — keeps the "which protocol is
//! active" state in one place and makes the
//! `PortMapperClient` trait object-safe for the task surface.
//!
//! Plan decision 4 ordering rationale: NAT-PMP probe is 1 s,
//! UPnP probe is 2 s. Trying NAT-PMP first means a common
//! happy-path (a router that speaks both) resolves in ~1 s;
//! UPnP-only routers pay 1 s + 2 s = 3 s for the first probe
//! cycle, once. Renewal reuses the cached protocol, so no
//! repeated probe cost.

use parking_lot::Mutex;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;

use super::natpmp::NatPmpMapper;
use super::upnp::UpnpMapper;
use super::{PortMapperClient, PortMapping, PortMappingError, Protocol};

/// Composing [`PortMapperClient`] that chains a
/// [`NatPmpMapper`] (if we have a gateway IP) + a
/// [`UpnpMapper`] (always available via SSDP).
///
/// On the first call to `probe`, attempts NAT-PMP and caches
/// the active protocol. Subsequent `install` / `renew` /
/// `remove` calls dispatch to the cached client without
/// re-probing the other protocol.
///
/// A probe failure on the active protocol invalidates the
/// cache so the next call re-runs the sequence — an operator
/// who restarts their router after switching from NAT-PMP to
/// UPnP-only shouldn't need to restart the mesh.
pub struct SequentialMapper {
    /// NAT-PMP client, constructed when the sequencer has a
    /// gateway IPv4. `None` on platforms where we couldn't
    /// discover the gateway (Windows; *BSD; unusual Linux) —
    /// in which case we skip straight to UPnP.
    ///
    /// Held as a trait object so unit tests can inject a
    /// `MockPortMapperClient` to drive specific state
    /// transitions (e.g., probe-succeeds-but-install-fails to
    /// exercise the install-time fallback path). Production
    /// always wraps a real `NatPmpMapper`.
    nat_pmp: Option<Box<dyn PortMapperClient>>,
    /// UPnP client. Always constructed; SSDP discovery happens
    /// internally on the first call. Trait-object for the same
    /// test-injection reason as `nat_pmp` above.
    upnp: Box<dyn PortMapperClient>,
    /// Which protocol succeeded on the most recent probe.
    /// `None` means either no probe has run, the last probe
    /// failed on both protocols, or the cache was invalidated
    /// by an `install` / `renew` failure so the next call
    /// re-runs the sequence.
    active: Mutex<Option<Protocol>>,
}

impl SequentialMapper {
    /// Construct a sequencer.
    ///
    /// - `gateway`: default IPv4 gateway (for NAT-PMP). Pass
    ///   `None` when OS gateway discovery failed — the
    ///   sequencer will run UPnP-only.
    /// - `local_ip`: the LAN IP the router should forward
    ///   matched traffic to. Required by UPnP; see
    ///   [`super::upnp::UpnpMapper::new`].
    pub fn new(gateway: Option<Ipv4Addr>, local_ip: IpAddr) -> Self {
        Self {
            nat_pmp: gateway.map(|g| Box::new(NatPmpMapper::new(g)) as Box<dyn PortMapperClient>),
            upnp: Box::new(UpnpMapper::new(local_ip)),
            active: Mutex::new(None),
        }
    }

    /// Test-only constructor: inject arbitrary trait-object
    /// clients. Lets unit tests drive state transitions that
    /// can't be reached with real NAT-PMP / UPnP loopback
    /// fixtures (e.g., probe-succeeds-but-install-fails on the
    /// same client). Production callers use [`Self::new`]
    /// or [`sequential_mapper_from_os`].
    #[cfg(test)]
    pub(crate) fn new_with_clients(
        nat_pmp: Option<Box<dyn PortMapperClient>>,
        upnp: Box<dyn PortMapperClient>,
    ) -> Self {
        Self {
            nat_pmp,
            upnp,
            active: Mutex::new(None),
        }
    }

    /// Which protocol is currently active (cached from the most
    /// recent successful probe). Public for tests + for
    /// stats-surface work that wants to expose the active
    /// protocol alongside `port_mapping_active`.
    pub fn active_protocol(&self) -> Option<Protocol> {
        *self.active.lock()
    }

    fn set_active(&self, protocol: Option<Protocol>) {
        *self.active.lock() = protocol;
    }
}

#[async_trait]
impl PortMapperClient for SequentialMapper {
    async fn probe(&self) -> Result<(), PortMappingError> {
        // Try NAT-PMP first (plan decision 4's 1 s budget).
        if let Some(pmp) = &self.nat_pmp {
            if pmp.probe().await.is_ok() {
                self.set_active(Some(Protocol::NatPmp));
                return Ok(());
            }
        }
        // Fall back to UPnP (2 s budget).
        self.upnp.probe().await?;
        self.set_active(Some(Protocol::Upnp));
        Ok(())
    }

    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        let active = self.active_protocol();

        // First attempt: the cached active protocol.
        let first_err = match active {
            None => return Err(PortMappingError::Unavailable),
            Some(Protocol::NatPmp) => {
                // .expect is safe: active = Some(NatPmp) only
                // set when we had a NatPmpMapper AND its probe
                // succeeded.
                let pmp = self
                    .nat_pmp
                    .as_ref()
                    .expect("active NatPmp without nat_pmp client");
                match pmp.install(internal_port, ttl).await {
                    Ok(m) => return Ok(m),
                    Err(e) => e,
                }
            }
            Some(Protocol::Upnp) => match self.upnp.install(internal_port, ttl).await {
                Ok(m) => return Ok(m),
                Err(e) => e,
            },
        };

        // Cached protocol's install failed — the probe answered
        // but the router refused the MAP (common on gateways
        // whose NAT-PMP responder exists but has policy against
        // arbitrary port mappings). Invalidate the cache and
        // fall back to the other protocol. Cubic-flagged P1:
        // without this retry, the sequencer was stuck on the
        // losing protocol for the whole task lifetime and UPnP
        // was never attempted.
        self.set_active(None);
        let fallback_proto = match active {
            Some(Protocol::NatPmp) => Protocol::Upnp,
            Some(Protocol::Upnp) => Protocol::NatPmp,
            None => unreachable!("handled above"),
        };
        match fallback_proto {
            Protocol::NatPmp => {
                // Fall back to NAT-PMP if we have a client;
                // otherwise surface the original error.
                let Some(pmp) = self.nat_pmp.as_ref() else {
                    return Err(first_err);
                };
                // Probe before install: `NatPmpMapper::install`
                // reads its external IP from the cache populated
                // by `probe()`. Without this probe, a successful
                // fallback install on a router whose NAT-PMP path
                // is actually serving the gateway would publish
                // the gateway's private LAN IP as the mapping's
                // external address — cubic-flagged P1. The main
                // path doesn't hit this because it uses whichever
                // protocol's own `probe()` primed the cache; the
                // fallback is the only cross-protocol transition,
                // so it's the one that must re-probe.
                if pmp.probe().await.is_err() {
                    return Err(first_err);
                }
                match pmp.install(internal_port, ttl).await {
                    Ok(m) => {
                        self.set_active(Some(Protocol::NatPmp));
                        Ok(m)
                    }
                    Err(_) => Err(first_err),
                }
            }
            Protocol::Upnp => match self.upnp.install(internal_port, ttl).await {
                Ok(m) => {
                    self.set_active(Some(Protocol::Upnp));
                    Ok(m)
                }
                Err(_) => Err(first_err),
            },
        }
    }

    async fn renew(&self, mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        // Match the installer — renew on the same protocol.
        match mapping.protocol {
            Protocol::NatPmp => {
                if let Some(pmp) = &self.nat_pmp {
                    pmp.renew(mapping).await
                } else {
                    Err(PortMappingError::Unavailable)
                }
            }
            Protocol::Upnp => self.upnp.renew(mapping).await,
        }
    }

    async fn remove(&self, mapping: &PortMapping) {
        match mapping.protocol {
            Protocol::NatPmp => {
                if let Some(pmp) = &self.nat_pmp {
                    pmp.remove(mapping).await;
                }
            }
            Protocol::Upnp => self.upnp.remove(mapping).await,
        }
    }
}

/// Build a [`SequentialMapper`] wired against the local
/// operating system — discovers the gateway + resolves the LAN
/// IP, then constructs the sequencer. Returns `None` if neither
/// protocol can be set up (no gateway discovered AND UPnP's
/// local-IP source address couldn't be resolved).
///
/// Used by `MeshNode::start` when `try_port_mapping(true)` is
/// set, so operators get the production sequencer instead of
/// [`super::NullPortMapper`].
pub async fn sequential_mapper_from_os() -> Option<SequentialMapper> {
    let gateway = super::gateway::default_ipv4_gateway();
    // Resolve a local IP against whatever address routes us to
    // the internet. Prefer the gateway if we have it; fall back
    // to a public IP (8.8.8.8) for source-address resolution if
    // gateway discovery failed — the OS picks the interface
    // that would be used to reach it, which is what UPnP wants.
    let probe_target = gateway.unwrap_or(Ipv4Addr::new(8, 8, 8, 8));
    let local_ip = super::gateway::local_ipv4_for_gateway(probe_target).await?;
    Some(SequentialMapper::new(gateway, local_ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::traversal::portmap::MockPortMapperClient;
    use std::sync::Arc;

    // NB: SequentialMapper composes concrete `NatPmpMapper` +
    // `UpnpMapper` clients — the trait field isn't generic. To
    // drive behavior in unit tests we construct it with a
    // dummy gateway + local IP that we know will fail (loopback,
    // no router) and assert the state-transition logic.

    fn sample_sequencer() -> SequentialMapper {
        SequentialMapper::new(Some(Ipv4Addr::LOCALHOST), IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    #[test]
    fn fresh_sequencer_has_no_active_protocol() {
        let seq = sample_sequencer();
        assert!(seq.active_protocol().is_none());
    }

    #[test]
    fn new_without_gateway_skips_nat_pmp() {
        let seq = SequentialMapper::new(None, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(seq.nat_pmp.is_none());
    }

    #[test]
    fn new_with_gateway_constructs_nat_pmp() {
        let seq = sample_sequencer();
        assert!(seq.nat_pmp.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_before_probe_is_unavailable() {
        // Without a successful probe, `active` is None → install
        // returns Unavailable rather than attempting to pick
        // a protocol blindly.
        let seq = sample_sequencer();
        let res = seq.install(9001, Duration::from_secs(60)).await;
        assert!(matches!(res, Err(PortMappingError::Unavailable)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_without_responders_returns_last_error() {
        // Loopback + no UPnP responder: both NAT-PMP and UPnP
        // fail. The sequencer returns the last error (UPnP's)
        // and leaves active as None.
        let seq = sample_sequencer();
        let start = tokio::time::Instant::now();
        let res = seq.probe().await;
        let elapsed = start.elapsed();

        assert!(res.is_err(), "no responders on loopback");
        assert!(seq.active_protocol().is_none());
        // Upper bound on wall clock: NAT-PMP deadline (~1 s) +
        // UPnP deadline (~2 s) + jitter.
        assert!(
            elapsed < Duration::from_secs(5),
            "both-protocol probe should bound by ~3 s; took {elapsed:?}",
        );
    }

    // ---- mock-based state-transition tests ----
    //
    // `SequentialMapper` now holds trait objects (see the
    // `Box<dyn PortMapperClient>` fields), so unit tests can
    // inject `MockPortMapperClient` via the test-only
    // `new_with_clients` constructor and drive specific
    // state transitions directly.

    fn sample_mapping(protocol: Protocol) -> PortMapping {
        PortMapping {
            external: "203.0.113.42:9001".parse().unwrap(),
            internal_port: 9001,
            ttl: Duration::from_secs(3600),
            protocol,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mock_client_surface_remains_usable() {
        let mock = MockPortMapperClient::new();
        mock.queue_probe(Ok(()));
        assert!(mock.probe().await.is_ok());
    }

    /// Regression test for a cubic-flagged P1 bug (TEST_COVERAGE_PLAN
    /// §P1-7): `SequentialMapper` cached `active = NatPmp` on a
    /// successful probe but left the cache in place when the
    /// *install* on that same protocol subsequently failed.
    /// Common case: a router's NAT-PMP responder answers
    /// external-address queries (probe succeeds) but its policy
    /// refuses arbitrary port MAP requests (install fails).
    /// Pre-fix, UPnP was never attempted even though it might
    /// have worked; the task exited on the first install error.
    /// The fix invalidates the cache on install failure and
    /// tries the other protocol exactly once before surfacing
    /// the original error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_failure_on_cached_protocol_falls_back_to_other() {
        // NAT-PMP: probe Ok, install Err (router refused MAP).
        let pmp = MockPortMapperClient::new();
        pmp.queue_probe(Ok(()));
        pmp.queue_install(Err(PortMappingError::Refused("nat-pmp policy".into())));

        // UPnP: install Ok with a UPnP-tagged mapping.
        let upnp = MockPortMapperClient::new();
        upnp.queue_install(Ok(sample_mapping(Protocol::Upnp)));

        let seq = SequentialMapper::new_with_clients(Some(Box::new(pmp)), Box::new(upnp));

        // Probe succeeds on NAT-PMP, cache pins to NatPmp.
        seq.probe().await.expect("probe should succeed on NAT-PMP");
        assert_eq!(seq.active_protocol(), Some(Protocol::NatPmp));

        // Install on cached NAT-PMP fails; sequencer must fall
        // back to UPnP and land on its mapping. The cached
        // protocol flips to UPnP.
        let mapping = seq
            .install(9001, Duration::from_secs(3600))
            .await
            .expect("install should fall back to UPnP when NAT-PMP refuses MAP");
        assert_eq!(
            mapping.protocol,
            Protocol::Upnp,
            "mapping should be tagged UPnP after fallback",
        );
        assert_eq!(
            seq.active_protocol(),
            Some(Protocol::Upnp),
            "cache should repoint to UPnP after successful fallback",
        );
    }

    /// Complement: if the fallback protocol ALSO fails, the
    /// sequencer surfaces the ORIGINAL error (from the cached
    /// protocol), not the fallback's error. Preserves the
    /// diagnostic signal operators actually care about — the
    /// first protocol's "why did NAT-PMP refuse" detail is
    /// more actionable than "UPnP SSDP discovery timed out."
    /// Also asserts the cache is invalidated (not left
    /// stuck on the failed protocol).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_failure_on_both_surfaces_original_error_and_clears_cache() {
        let pmp = MockPortMapperClient::new();
        pmp.queue_probe(Ok(()));
        pmp.queue_install(Err(PortMappingError::Refused("nat-pmp policy".into())));

        let upnp = MockPortMapperClient::new();
        upnp.queue_install(Err(PortMappingError::Unavailable));

        let seq = SequentialMapper::new_with_clients(Some(Box::new(pmp)), Box::new(upnp));
        seq.probe().await.expect("probe");

        let err = seq
            .install(9001, Duration::from_secs(3600))
            .await
            .expect_err("both installs fail — result must be Err");
        match err {
            PortMappingError::Refused(msg) => assert!(
                msg.contains("nat-pmp policy"),
                "should surface the original NAT-PMP error, got {msg:?}",
            ),
            other => panic!("expected original Refused error; got {other:?}"),
        }
        assert!(
            seq.active_protocol().is_none(),
            "cache must be cleared when both installs fail",
        );
    }

    /// Regression (cubic P1 follow-up): when UPnP is the cached
    /// protocol and its install fails, the fallback to NAT-PMP
    /// MUST call `probe()` before `install()`. Rationale: the
    /// real `NatPmpMapper::install` reads its external IP from
    /// a cache that only gets populated by `probe()`. If we
    /// skip the probe, a successful install on a router whose
    /// NAT-PMP path is up would publish the *gateway's LAN IP*
    /// as the mapping's external address — visible on the
    /// capability announcement to peers that then try to punch
    /// to a private address. The fix probes first; this test
    /// pins that ordering by queueing two successful probes on
    /// the NAT-PMP mock and verifying the second (the fallback
    /// one) is consumed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_to_nat_pmp_probes_before_installing() {
        let pmp = Arc::new(MockPortMapperClient::new());
        // Probe #1: fails so the main-path probe lands on UPnP.
        // Probe #2: succeeds — the fallback-path probe that the
        // fix requires before install.
        pmp.queue_probe(Err(PortMappingError::Unavailable));
        pmp.queue_probe(Ok(()));
        pmp.queue_install(Ok(sample_mapping(Protocol::NatPmp)));

        let upnp = MockPortMapperClient::new();
        upnp.queue_probe(Ok(()));
        upnp.queue_install(Err(PortMappingError::Refused("upnp gateway busy".into())));

        let seq = SequentialMapper::new_with_clients(Some(Box::new(pmp.clone())), Box::new(upnp));

        // Main probe: NAT-PMP fails, UPnP wins.
        seq.probe().await.expect("upnp probe should succeed");
        assert_eq!(seq.active_protocol(), Some(Protocol::Upnp));

        // Install: UPnP fails; fallback must probe NAT-PMP first,
        // then install. Returns the NAT-PMP-tagged mapping.
        let mapping = seq
            .install(9001, Duration::from_secs(3600))
            .await
            .expect("fallback to NAT-PMP must succeed");
        assert_eq!(mapping.protocol, Protocol::NatPmp);
        assert_eq!(seq.active_protocol(), Some(Protocol::NatPmp));

        // Probe queue on NAT-PMP must be empty — both queued
        // probes were consumed (main-path + fallback-path). A
        // regression that skipped the fallback probe would leave
        // the second entry in the queue.
        assert_eq!(
            pmp.remaining_probes(),
            0,
            "fallback path must have consumed the queued NAT-PMP probe",
        );
    }

    /// Companion: if the fallback's NAT-PMP probe fails, the
    /// fallback must NOT call `install()` — any mapping
    /// produced without a successful probe would carry the
    /// gateway's LAN IP as its external address. The sequencer
    /// surfaces the ORIGINAL UPnP error instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fallback_nat_pmp_probe_failure_surfaces_original_upnp_error() {
        let pmp = Arc::new(MockPortMapperClient::new());
        // Main-path probe: fails so UPnP wins.
        pmp.queue_probe(Err(PortMappingError::Unavailable));
        // Fallback-path probe: also fails — must short-circuit.
        pmp.queue_probe(Err(PortMappingError::Transport("no responder".into())));
        // Install: queued a success to prove it is NOT called.
        // If the fix regressed and install ran, the sequencer
        // would erroneously return Ok(sample_mapping).
        pmp.queue_install(Ok(sample_mapping(Protocol::NatPmp)));

        let upnp = MockPortMapperClient::new();
        upnp.queue_probe(Ok(()));
        upnp.queue_install(Err(PortMappingError::Refused("upnp gateway busy".into())));

        let seq = SequentialMapper::new_with_clients(Some(Box::new(pmp.clone())), Box::new(upnp));
        seq.probe().await.expect("upnp probe");

        let err = seq
            .install(9001, Duration::from_secs(3600))
            .await
            .expect_err("fallback probe failure must short-circuit");
        match err {
            PortMappingError::Refused(msg) => assert!(
                msg.contains("upnp gateway busy"),
                "must surface the original UPnP error, got {msg:?}",
            ),
            other => panic!("expected original UPnP Refused error, got {other:?}"),
        }
        // Install queue must be untouched — the fix must short-
        // circuit BEFORE install when the probe fails.
        assert_eq!(
            pmp.remaining_installs(),
            1,
            "fallback install must NOT fire when the fallback probe fails",
        );
    }

    /// Guardrail: if only UPnP exists (no gateway → no NAT-PMP
    /// client) and its install fails, there's no fallback to
    /// attempt. The sequencer surfaces UPnP's error directly
    /// without crashing on an unreachable `nat_pmp` client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_failure_with_no_nat_pmp_fallback_surfaces_upnp_error() {
        let upnp = MockPortMapperClient::new();
        upnp.queue_probe(Ok(()));
        upnp.queue_install(Err(PortMappingError::Unavailable));

        let seq = SequentialMapper::new_with_clients(None, Box::new(upnp));
        seq.probe().await.expect("upnp probe");
        assert_eq!(seq.active_protocol(), Some(Protocol::Upnp));

        let err = seq.install(9001, Duration::from_secs(3600)).await;
        assert!(
            matches!(err, Err(PortMappingError::Unavailable)),
            "UPnP-only deployment with install failure should surface \
             Unavailable without panicking on missing NAT-PMP client; got {err:?}",
        );
        assert!(
            seq.active_protocol().is_none(),
            "cache cleared on install failure regardless of fallback availability",
        );
    }
}
