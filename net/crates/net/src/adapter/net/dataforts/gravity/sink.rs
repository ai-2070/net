//! `HeatSink` — abstraction over the heat-tag emission path so
//! the greedy runtime can drive heat announcements without
//! depending directly on `MeshNode`.
//!
//! Mirrors the [`ChainTagSink`] pattern from
//! `replication_coordinator`. The production impl is `MeshNode`
//! (which routes through `announce_heat` / `withdraw_heat`); test
//! sinks record the call sequence for assertions.
//!
//! [`ChainTagSink`]: crate::adapter::net::redex::ChainTagSink

use crate::error::AdapterError;

/// Wire-side surface for heat-tag emissions. Async + Send so the
/// gravity tick can be driven from a tokio task.
#[async_trait::async_trait]
pub trait HeatSink: Send + Sync {
    /// Emit (or replace) the `heat:<hex>=<rate>` reserved tag for
    /// `origin_hash`. Idempotent — the most recent call wins.
    /// Rate clamping (substrate enforces `[0.0, 1.0]`) happens
    /// inside the impl.
    async fn announce_heat(&self, origin_hash: u64, rate: f64) -> Result<(), AdapterError>;

    /// Withdraw every `heat:<hex>=*` tag for `origin_hash`.
    /// Idempotent. Peers drop the heat annotation; the chain's
    /// `causal:` advertisements stay.
    async fn withdraw_heat(&self, origin_hash: u64) -> Result<(), AdapterError>;
}
