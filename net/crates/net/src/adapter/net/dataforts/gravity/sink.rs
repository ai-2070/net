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

    /// Apply a batch of `(origin_hash, Option<rate>)` updates in a
    /// single round-trip. `Some(rate)` is emit/replace; `None` is
    /// withdraw. The gravity tick uses this to coalesce per-chain
    /// emissions into one capability rebroadcast — without it the
    /// per-tick wire cost was O(n_chains × n_tags).
    ///
    /// Default impl falls back to the per-chain methods so existing
    /// `HeatSink` impls keep working; production impls override.
    async fn announce_heat_batch(
        &self,
        updates: &[(u64, Option<f64>)],
    ) -> Result<(), AdapterError> {
        for &(origin_hash, rate_opt) in updates {
            match rate_opt {
                Some(rate) => self.announce_heat(origin_hash, rate).await?,
                None => self.withdraw_heat(origin_hash).await?,
            }
        }
        Ok(())
    }
}
