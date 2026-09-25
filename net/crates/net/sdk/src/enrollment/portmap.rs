// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keep a router-side TCP port mapping for the enrollment listener.
//!
//! The mesh maps its own UDP port (`MeshBuilder::try_port_mapping`). The
//! PSK-free enrollment listener is TCP on the same port number, so a node that
//! must be reachable without manual router configuration also needs a TCP
//! mapping. [`TcpMapping`] asks the local gateway over NAT-PMP / PCP, then
//! UPnP-IGD, renews on an interval, and removes the mapping on shutdown.
//!
//! Opportunistic: `None` from [`TcpMapping::establish`] means no gateway
//! granted a mapping (none speaks the protocols, it refused, or it timed out).
//! A mapping only says the router forwards the port; it is not evidence that the
//! address is reachable from any particular network.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::traversal::portmap::{
    sequential_mapper_from_os_for, MapTransport, PortMapperClient, PortMapping,
};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Lease requested from the router for each install / renewal.
pub const TCP_MAPPING_LEASE: Duration = Duration::from_secs(2 * 60 * 60);
/// Renewal interval; well inside the lease.
pub const TCP_MAPPING_RENEWAL: Duration = Duration::from_secs(30 * 60);
/// Consecutive renewal failures after which the mapping is abandoned.
pub const TCP_MAPPING_MAX_FAILURES: u32 = 3;

/// A live TCP port mapping, renewed in the background until shutdown.
pub struct TcpMapping {
    external: Arc<Mutex<Option<SocketAddr>>>,
    stop: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl core::fmt::Debug for TcpMapping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TcpMapping")
            .field("external", &*self.external.lock())
            .finish()
    }
}

impl TcpMapping {
    /// Discover the default gateway and map `internal_port` over TCP.
    pub async fn establish(internal_port: u16) -> Option<Self> {
        let client = sequential_mapper_from_os_for(MapTransport::Tcp).await?;
        Self::establish_with(Box::new(client), internal_port, TCP_MAPPING_RENEWAL).await
    }

    /// [`Self::establish`] with an explicit client and renewal interval.
    pub async fn establish_with(
        client: Box<dyn PortMapperClient>,
        internal_port: u16,
        renew_every: Duration,
    ) -> Option<Self> {
        client.probe().await.ok()?;
        let mapping = client
            .install(internal_port, TCP_MAPPING_LEASE)
            .await
            .ok()?;
        let external = Arc::new(Mutex::new(Some(mapping.external)));
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(keep(
            client,
            mapping,
            renew_every,
            external.clone(),
            stop.clone(),
        ));
        Some(Self {
            external,
            stop,
            task: Some(task),
        })
    }

    /// Current external address, or `None` once the mapping was abandoned
    /// after repeated renewal failures.
    pub fn external(&self) -> Option<SocketAddr> {
        *self.external.lock()
    }

    /// Remove the mapping from the router and stop renewing.
    pub async fn shutdown(mut self) {
        self.stop.notify_one();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for TcpMapping {
    fn drop(&mut self) {
        // Best effort: ask the renewal task to remove the mapping. Without an
        // awaited shutdown the router drops it when the lease expires.
        self.stop.notify_one();
    }
}

async fn keep(
    client: Box<dyn PortMapperClient>,
    mut current: PortMapping,
    renew_every: Duration,
    external: Arc<Mutex<Option<SocketAddr>>>,
    stop: Arc<Notify>,
) {
    let mut failures = 0u32;
    loop {
        tokio::select! {
            _ = stop.notified() => {
                client.remove(&current).await;
                *external.lock() = None;
                return;
            }
            _ = tokio::time::sleep(renew_every) => match client.renew(&current).await {
                Ok(renewed) => {
                    failures = 0;
                    *external.lock() = Some(renewed.external);
                    current = renewed;
                }
                Err(_) => {
                    failures += 1;
                    if failures >= TCP_MAPPING_MAX_FAILURES {
                        client.remove(&current).await;
                        *external.lock() = None;
                        return;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::traversal::portmap::{MockPortMapperClient, PortMappingError, Protocol};

    fn mapping(port: u16) -> PortMapping {
        PortMapping {
            external: SocketAddr::from(([198, 51, 100, 7], port)),
            internal_port: 7443,
            ttl: TCP_MAPPING_LEASE,
            protocol: Protocol::NatPmp,
        }
    }

    #[tokio::test]
    async fn no_gateway_means_no_mapping() {
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Err(PortMappingError::Unavailable));
        assert!(
            TcpMapping::establish_with(Box::new(mock), 7443, TCP_MAPPING_RENEWAL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn installs_renews_and_removes_on_shutdown() {
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Ok(()));
        mock.queue_install(Ok(mapping(7443)));
        for _ in 0..50 {
            mock.queue_renew(Ok(mapping(7444)));
        }
        let keeper =
            TcpMapping::establish_with(Box::new(mock.clone()), 7443, Duration::from_millis(20))
                .await
                .unwrap();
        assert_eq!(keeper.external(), Some(mapping(7443).external));
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The renewed (router-changed) address is what is reported.
        assert_eq!(keeper.external().map(|a| a.port()), Some(7444));
        keeper.shutdown().await;
        assert_eq!(mock.remove_call_count(), 1);
    }

    #[tokio::test]
    async fn repeated_renewal_failures_abandon_and_remove_the_mapping() {
        let mock = Arc::new(MockPortMapperClient::new());
        mock.queue_probe(Ok(()));
        mock.queue_install(Ok(mapping(7443)));
        // Empty renew queue: every renewal fails.
        let keeper =
            TcpMapping::establish_with(Box::new(mock.clone()), 7443, Duration::from_millis(10))
                .await
                .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(keeper.external(), None);
        assert_eq!(mock.remove_call_count(), 1);
        keeper.shutdown().await;
    }
}
