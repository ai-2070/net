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

/// Wire-side surface for blob heat-tag emissions. Mirrors
/// [`HeatSink`] but keys on the chunk's 32-byte BLAKE3 hash
/// rather than the chain's `u64` `origin_hash`. PR-5j-c
/// foundation for the gravity migration controller.
///
/// Wire form: `heat:blob:<hex64>=<rate>` reserved tag. The
/// `heat:` prefix matches the chain-heat shape; the `blob:`
/// body sub-prefix distinguishes the per-chunk projection.
/// Mesh integrators implement this against a `MeshNode` whose
/// `announce_capabilities` rebroadcast carries the tags forward.
#[async_trait::async_trait]
pub trait BlobHeatSink: Send + Sync {
    /// Emit (or replace) the `heat:blob:<hex>=<rate>` reserved
    /// tag for chunk `hash`. Idempotent — most recent call wins.
    async fn announce_blob_heat(&self, hash: [u8; 32], rate: f64) -> Result<(), AdapterError>;

    /// Withdraw every `heat:blob:<hex>=*` tag for chunk `hash`.
    /// Idempotent; mirrors `HeatSink::withdraw_heat`.
    async fn withdraw_blob_heat(&self, hash: [u8; 32]) -> Result<(), AdapterError>;

    /// Batched form for coalescing a tick's worth of emissions
    /// into one capability rebroadcast. Default impl falls back
    /// to the per-hash methods; production impls override to
    /// hold the rebroadcast until every update has landed.
    async fn announce_blob_heat_batch(
        &self,
        updates: &[([u8; 32], Option<f64>)],
    ) -> Result<(), AdapterError> {
        for &(hash, rate_opt) in updates {
            match rate_opt {
                Some(rate) => self.announce_blob_heat(hash, rate).await?,
                None => self.withdraw_blob_heat(hash).await?,
            }
        }
        Ok(())
    }
}
