//! MeshOS SDK — daemon-author surface.
//!
//! This is the customer-facing entry point for writing daemons
//! that participate in MeshOS supervision. The actual
//! implementation lives in the substrate at
//! `net::adapter::net::behavior::meshos::sdk`; this module
//! re-exports the SDK types under a clean `net_sdk::meshos::*`
//! path so consumers don't reach into substrate internals.
//!
//! # Surface
//!
//! - [`MeshOsDaemonSdk`] — one-call entry point. Wraps a
//!   [`MeshOsRuntime`] with daemon-control routing; provides
//!   `register_daemon(...) -> MeshOsDaemonHandle`.
//! - [`MeshOsDaemonHandle`] — per-daemon handle. Owns the
//!   control-event receiver, capability-publish surface,
//!   graceful-shutdown sequence, and read-only metadata view.
//! - [`MetadataView`] / [`MaintenanceStateView`] — read-only
//!   cluster context the daemon can observe.
//! - [`SdkError`] — operator-readable error surface with the
//!   `<<meshos-sdk-kind:KIND>>MSG` discriminator format every
//!   cross-language SDK uses.
//! - Re-exported substrate types: [`DaemonControl`],
//!   [`DaemonHealth`], [`MeshDaemon`], [`CapabilitySet`],
//!   [`EntityKeypair`], [`MeshOsConfig`], [`MeshOsRuntime`].
//!
//! # Daemon-author quickstart
//!
//! ```ignore
//! use net_sdk::meshos::{
//!     CapabilitySet, DaemonControl, DaemonHealth, EntityKeypair, MeshDaemon,
//!     MeshOsConfig, MeshOsDaemonSdk,
//! };
//! use std::sync::Arc;
//!
//! struct TelemetryDaemon { /* … */ }
//!
//! impl MeshDaemon for TelemetryDaemon {
//!     fn name(&self) -> &str { "telemetry" }
//!     fn requirements(&self) -> _ { Default::default() }
//!     fn process(&mut self, _event: &_) -> _ { Ok(vec![]) }
//!     fn health(&self) -> DaemonHealth { DaemonHealth::Healthy }
//! }
//!
//! # async fn run(dispatcher: Arc<impl _>) -> Result<(), Box<dyn std::error::Error>> {
//! let sdk = MeshOsDaemonSdk::start(MeshOsConfig::default(), dispatcher);
//! let mut handle = sdk.register_daemon(
//!     Box::new(TelemetryDaemon { /* … */ }),
//!     EntityKeypair::generate(),
//! )?;
//!
//! while let Some(ev) = handle.next_control().await {
//!     match ev {
//!         DaemonControl::Shutdown { .. } => break,
//!         DaemonControl::BackpressureOn { level } => { /* throttle */ }
//!         _ => {}
//!     }
//! }
//!
//! handle.graceful_shutdown(std::time::Duration::from_secs(5)).await?;
//! sdk.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Macro convenience
//!
//! The substrate re-exports a [`daemon_main!`] macro that
//! collapses the boilerplate into a single block — see the
//! macro's own documentation in
//! `net::adapter::net::behavior::meshos::sdk::daemon_main`.
//!
//! # Non-goals
//!
//! Per `MESHOS_SDK_PLAN.md`'s locked decisions, this SDK is
//! **daemon-side only**:
//!
//! - No placement / replica / scheduler APIs.
//! - No admin-event issuance (drain, cordon, maintenance, etc).
//! - No "control MeshOS" surfaces (avoid lists, backpressure
//!   tuning, scheduler config).
//! - No federated-execution / MeshDB-query plumbing — those
//!   belong to the (forthcoming) Deck SDK.
//!
//! Operator tooling lives in `DECK_SDK_PLAN.md`'s surface.

// Re-export the substrate-side SDK types under a clean
// `net_sdk::meshos::*` path. The implementation lives in
// `net::adapter::net::behavior::meshos::sdk` — this module is
// the customer-facing seam.
pub use net::adapter::net::behavior::meshos::{
    DaemonControlRouter, MaintenanceStateView, MeshOsDaemonHandle, MeshOsDaemonSdk, MetadataView,
    SdkError, SdkRoutingDispatcher, DEFAULT_CONTROL_CHANNEL_CAPACITY, DEFAULT_GRACEFUL_SHUTDOWN,
};

// Supporting types daemon authors need.
pub use net::adapter::net::behavior::capability::CapabilitySet;
pub use net::adapter::net::behavior::meshos::{
    ActionDispatcher, DispatchError, LogLevel, LogLine, MeshOsConfig, MeshOsRuntime,
    RuntimeShutdownError, RuntimeStats,
};
pub use net::adapter::net::compute::{
    DaemonControl, DaemonError, DaemonHealth, DaemonHostConfig, MeshDaemon,
};
pub use net::adapter::net::EntityKeypair;
