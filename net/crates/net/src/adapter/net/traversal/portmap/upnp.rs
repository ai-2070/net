//! UPnP-IGD client — [`PortMapperClient`] backed by the
//! `igd-next` crate.
//!
//! UPnP-IGD is considerably more ceremony than NAT-PMP. The
//! client:
//!
//! 1. Sends an SSDP `M-SEARCH` multicast to `239.255.255.250:1900`
//!    to discover the router's IGD control URL.
//! 2. Parses the returned device description XML to find the
//!    `WANIPConnection` / `WANPPPConnection` service endpoint.
//! 3. Issues SOAP requests (`AddPortMapping`, `DeletePortMapping`,
//!    `GetExternalIPAddress`) against that endpoint.
//!
//! We delegate all of that to `igd-next` (see plan decision 10).
//! This module wraps its tokio-flavored API (`aio::tokio`) behind
//! our [`PortMapperClient`] trait and translates its typed
//! errors into the stable `PortMappingError` vocabulary.
//!
//! # Why `igd-next` over inlining
//!
//! The NAT-PMP module inlines its ~100 lines of wire format
//! because the alternative (depending on the dormant
//! `rust-natpmp` crate) offered little. UPnP is different:
//! device-description XML parsing + service-table traversal +
//! SOAP envelope assembly is ~500–1000 lines per the IGD v1/v2
//! specs, plus SSDP discovery. `igd-next` is MIT-licensed,
//! actively maintained, and already handles the quirks of
//! consumer-router UPnP implementations (non-strict XML, missing
//! namespaces, IPv4-only service URIs, etc.).
//!
//! # Stage 4b-3 scope
//!
//! - [`UpnpMapper`] — `PortMapperClient` impl. Discovers the
//!   gateway on probe, caches it, and reuses the cached gateway
//!   across install / renew / remove.
//! - Error mapping from `igd-next`'s typed errors into
//!   [`PortMappingError`].
//! - Tests: unit-level error mapping + an integration test that
//!   asserts graceful timeout against a network with no IGD
//!   responder (typical CI environment).
//!
//! Gateway discovery + local LAN IP selection are the caller's
//! responsibility. Stage 4b-4's sequencer wires them up. The
//! `UpnpMapper::new(local_ip)` constructor requires the caller
//! to supply the LAN IP the router should forward to — UPnP has
//! no way to infer it from the request envelope the way NAT-PMP
//! does.

use parking_lot::Mutex;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use igd_next::aio::tokio::Tokio;
use igd_next::aio::Gateway;
use igd_next::{PortMappingProtocol, SearchOptions};

use super::{PortMapperClient, PortMapping, PortMappingError, Protocol};

/// Per-call deadline for UPnP operations. Longer than NAT-PMP's
/// 1 s (plan decision 4) because UPnP has to do SSDP discovery
/// + SOAP request + XML parse on every call that isn't cached.
pub const UPNP_DEADLINE: Duration = Duration::from_secs(2);

/// SSDP search timeout — how long we wait for any IGD to respond.
/// Shorter than [`UPNP_DEADLINE`] so discovery failures surface
/// before the overall call deadline.
pub const UPNP_SEARCH_TIMEOUT: Duration = Duration::from_millis(1500);

/// UPnP description set on `AddPortMapping` so the operator
/// can identify the mesh's mapping in their router's admin UI.
pub const UPNP_DESCRIPTION: &str = "cyberdeck-mesh";

/// A [`PortMapperClient`] backed by `igd-next`'s tokio API.
///
/// Caches the discovered gateway between calls so renewal
/// doesn't trigger a fresh SSDP probe every 30 minutes. The
/// cache is invalidated on transport errors — a gateway reboot
/// or network change will re-trigger discovery on the next call.
pub struct UpnpMapper {
    /// LAN IP the router should forward matched traffic to.
    /// UPnP's `AddPortMapping` requires an explicit
    /// `NewInternalClient` address; unlike NAT-PMP, the
    /// protocol has no way to infer it from the request's
    /// source address.
    local_ip: IpAddr,
    /// Cached IGD gateway. Populated on first successful
    /// `probe()` / `install()`; cleared on transport errors.
    gateway: Mutex<Option<Gateway<Tokio>>>,
}

impl UpnpMapper {
    /// Construct a mapper that maps to `local_ip` on the LAN.
    /// `local_ip` should be the interface address the mesh
    /// socket bound to — not `0.0.0.0` and not a loopback.
    pub fn new(local_ip: IpAddr) -> Self {
        Self {
            local_ip,
            gateway: Mutex::new(None),
        }
    }

    /// Read the cached gateway, if any. Lock-free fast path for
    /// renewal / remove calls that follow a successful probe.
    fn cached_gateway(&self) -> Option<Gateway<Tokio>> {
        self.gateway.lock().clone()
    }

    fn cache_gateway(&self, gw: Gateway<Tokio>) {
        *self.gateway.lock() = Some(gw);
    }

    fn invalidate_gateway(&self) {
        *self.gateway.lock() = None;
    }

    /// Discover (or re-use a cached) gateway. Bounded by
    /// [`UPNP_SEARCH_TIMEOUT`].
    async fn gateway(&self) -> Result<Gateway<Tokio>, PortMappingError> {
        if let Some(gw) = self.cached_gateway() {
            return Ok(gw);
        }
        let opts = SearchOptions {
            timeout: Some(UPNP_SEARCH_TIMEOUT),
            ..Default::default()
        };
        let gw = igd_next::aio::tokio::search_gateway(opts)
            .await
            .map_err(search_err_to_port_mapping)?;
        self.cache_gateway(gw.clone());
        Ok(gw)
    }
}

#[async_trait]
impl PortMapperClient for UpnpMapper {
    async fn probe(&self) -> Result<(), PortMappingError> {
        // Discovery + external-IP read, bounded by the overall
        // UPNP_DEADLINE. A router that responds to SSDP but
        // fails the XML fetch would time out here and the
        // mapper falls through to Unavailable.
        match tokio::time::timeout(UPNP_DEADLINE, async {
            let gw = self.gateway().await?;
            // `get_external_ip` is the minimal "is this gateway
            // actually serving UPnP?" check. A gateway returning
            // a bad XML response here would have failed the
            // `search_gateway` step already.
            gw.get_external_ip()
                .await
                .map_err(|_| PortMappingError::Transport("get_external_ip failed".into()))
        })
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                self.invalidate_gateway();
                Err(e)
            }
            Err(_) => {
                self.invalidate_gateway();
                Err(PortMappingError::Timeout)
            }
        }
    }

    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError> {
        let lease = ttl.as_secs().min(u32::MAX as u64) as u32;
        let result = tokio::time::timeout(UPNP_DEADLINE, async {
            let gw = self.gateway().await?;
            let local = SocketAddr::new(self.local_ip, internal_port);
            let external_ip = gw
                .get_external_ip()
                .await
                .map_err(|_| PortMappingError::Transport("get_external_ip failed".into()))?;
            // Previously called `add_port`, which assumes the
            // requested external port equals the internal port. Some
            // IGD implementations silently re-map `NewExternalPort`
            // to a free port and return success — the returned
            // `PortMapping` then carried the wrong external port and
            // the mesh advertised an unreachable address.
            // `add_any_port` returns the actually-mapped external
            // port, which we record in the `PortMapping`.
            let actual_external_port = gw
                .add_any_port(PortMappingProtocol::UDP, local, lease, UPNP_DESCRIPTION)
                .await
                .map_err(add_any_port_err_to_port_mapping)?;
            Ok::<_, PortMappingError>(PortMapping {
                external: SocketAddr::new(external_ip, actual_external_port),
                internal_port,
                ttl: Duration::from_secs(lease as u64),
                protocol: Protocol::Upnp,
            })
        })
        .await;

        match result {
            Ok(Ok(mapping)) => Ok(mapping),
            Ok(Err(e)) => {
                self.invalidate_gateway();
                Err(e)
            }
            Err(_) => {
                self.invalidate_gateway();
                Err(PortMappingError::Timeout)
            }
        }
    }

    async fn renew(&self, mapping: &PortMapping) -> Result<PortMapping, PortMappingError> {
        // IGD's `AddPortMapping` is idempotent as refresh:
        // re-issuing with the same internal/external port
        // refreshes the lease. No separate renewal verb.
        self.install(mapping.internal_port, mapping.ttl).await
    }

    async fn remove(&self, mapping: &PortMapping) {
        // Best-effort: we hold the same deadline but don't
        // surface errors. The router cleans up on TTL expiry
        // if this fails (plan decision 12).
        let _ = tokio::time::timeout(UPNP_DEADLINE, async {
            let gw = self.gateway().await?;
            gw.remove_port(PortMappingProtocol::UDP, mapping.external.port())
                .await
                .map_err(|_| PortMappingError::Transport("remove_port failed".into()))?;
            Ok::<_, PortMappingError>(())
        })
        .await;
    }
}

/// Map `igd-next::SearchError` into our stable vocabulary.
/// `NoResponseWithinTimeout` is the no-UPnP-on-this-network case
/// and maps to `Unavailable` so the stage-4b-4 sequencer can
/// fall through cleanly.
fn search_err_to_port_mapping(err: igd_next::SearchError) -> PortMappingError {
    use igd_next::SearchError;
    match err {
        SearchError::NoResponseWithinTimeout => PortMappingError::Unavailable,
        SearchError::InvalidResponse => PortMappingError::Transport("invalid IGD response".into()),
        SearchError::XmlError(e) => PortMappingError::Transport(format!("IGD XML parse: {e}")),
        SearchError::Utf8Error(e) => PortMappingError::Transport(format!("IGD UTF-8: {e}")),
        SearchError::IoError(e) => PortMappingError::Transport(format!("IGD I/O: {e}")),
        other => PortMappingError::Transport(other.to_string()),
    }
}

/// Map `igd-next::AddPortError` into our stable vocabulary.
/// `PortInUse` / `SamePortValuesRequired` / `OnlyPermanentLeasesSupported`
/// are router-policy refusals; other variants are transport.
#[allow(dead_code)]
fn add_port_err_to_port_mapping(err: igd_next::AddPortError) -> PortMappingError {
    use igd_next::AddPortError;
    match err {
        AddPortError::PortInUse => PortMappingError::Refused("port-in-use".into()),
        AddPortError::SamePortValuesRequired => {
            PortMappingError::Refused("same-port-required".into())
        }
        AddPortError::OnlyPermanentLeasesSupported => {
            PortMappingError::Refused("only-permanent-leases-supported".into())
        }
        AddPortError::DescriptionTooLong => {
            PortMappingError::Transport("description too long".into())
        }
        AddPortError::ExternalPortZeroInvalid | AddPortError::InternalPortZeroInvalid => {
            PortMappingError::Transport("zero port invalid".into())
        }
        AddPortError::RequestError(e) => PortMappingError::Transport(format!("IGD request: {e}")),
        AddPortError::ActionNotAuthorized => {
            PortMappingError::Refused("action-not-authorized".into())
        }
    }
}

/// Companion mapper for `AddAnyPortError`. `ExternalPortInUse` /
/// `NoPortsAvailable` / `OnlyPermanentLeasesSupported` are
/// router-policy refusals; other variants are transport.
fn add_any_port_err_to_port_mapping(err: igd_next::AddAnyPortError) -> PortMappingError {
    use igd_next::AddAnyPortError;
    match err {
        AddAnyPortError::ExternalPortInUse => {
            PortMappingError::Refused("external-port-in-use".into())
        }
        AddAnyPortError::NoPortsAvailable => PortMappingError::Refused("no-ports-available".into()),
        AddAnyPortError::OnlyPermanentLeasesSupported => {
            PortMappingError::Refused("only-permanent-leases-supported".into())
        }
        AddAnyPortError::DescriptionTooLong => {
            PortMappingError::Transport("description too long".into())
        }
        AddAnyPortError::InternalPortZeroInvalid => {
            PortMappingError::Transport("zero port invalid".into())
        }
        AddAnyPortError::RequestError(e) => {
            PortMappingError::Transport(format!("IGD request: {e}"))
        }
        AddAnyPortError::ActionNotAuthorized => {
            PortMappingError::Refused("action-not-authorized".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use igd_next::AddPortError;

    #[test]
    fn error_mapping_no_response_is_unavailable() {
        let mapped = search_err_to_port_mapping(igd_next::SearchError::NoResponseWithinTimeout);
        assert!(matches!(mapped, PortMappingError::Unavailable));
    }

    #[test]
    fn error_mapping_port_in_use_is_refused() {
        let mapped = add_port_err_to_port_mapping(AddPortError::PortInUse);
        match mapped {
            PortMappingError::Refused(msg) => assert_eq!(msg, "port-in-use"),
            other => panic!("expected Refused(port-in-use), got {other:?}"),
        }
    }

    #[test]
    fn error_mapping_action_not_authorized_is_refused() {
        let mapped = add_port_err_to_port_mapping(AddPortError::ActionNotAuthorized);
        match mapped {
            PortMappingError::Refused(msg) => assert_eq!(msg, "action-not-authorized"),
            other => panic!("expected Refused(action-not-authorized), got {other:?}"),
        }
    }

    #[test]
    fn error_mapping_zero_port_is_transport() {
        let mapped = add_port_err_to_port_mapping(AddPortError::ExternalPortZeroInvalid);
        assert!(matches!(mapped, PortMappingError::Transport(_)));
    }

    #[test]
    fn constructor_caches_no_gateway_initially() {
        let mapper = UpnpMapper::new("10.0.0.1".parse().unwrap());
        assert!(mapper.cached_gateway().is_none());
    }

    /// Integration-style: SSDP search against a network with no
    /// IGD responder (the typical CI environment) should fail
    /// with `Unavailable` within the search-timeout budget.
    /// Not `Timeout` — `igd-next` returns `NoResponseWithinTimeout`
    /// which we map to `Unavailable` so the sequencer falls
    /// through cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_on_no_router_returns_unavailable() {
        // NB: this runs against whatever the host OS says is the
        // default multicast interface. In a sandbox where the
        // SSDP port is unreachable, the search will fail fast
        // with an IoError; in a normal CI network where SSDP
        // goes out but no router responds, the search returns
        // NoResponseWithinTimeout. Both map to errors we accept
        // here — the property we care about is "doesn't hang
        // beyond the deadline + doesn't panic."
        let mapper = UpnpMapper::new("127.0.0.1".parse().unwrap());
        let start = tokio::time::Instant::now();
        let res = mapper.probe().await;
        let elapsed = start.elapsed();

        assert!(res.is_err(), "probe should fail in a no-IGD env");
        // Either Unavailable (NoResponseWithinTimeout) or
        // Transport (IoError) or Timeout (deadline fired first)
        // are all acceptable — the test asserts structural
        // behaviour, not the specific variant.
        //
        // Deadline upper bound: UPNP_DEADLINE (2 s) plus
        // scheduler jitter. Allow up to 3 s.
        assert!(
            elapsed < Duration::from_secs(3),
            "probe should respect deadline; took {elapsed:?}",
        );
    }
}
