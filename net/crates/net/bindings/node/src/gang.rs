//! napi bindings for the gang-claim GPU-island scheduler — methods on
//! the `NetMesh` class plus the flat criteria / island-input objects.
//!
//! The core `MatchCriteria` embeds a rich `CapabilityQuery` enum; the JS
//! boundary instead takes a flat [`GpuIslandCriteria`] and builds the
//! composite capability query + numeric filter + selection policy
//! internally, so callers never touch the internal enum shapes.

use ::net::adapter::net::behavior::fold::{CapabilityFilter, CapabilityQuery, GpuSet, IslandRecord};
use ::net::adapter::net::behavior::gang::{
    ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::common::bigint_u64;
use crate::NetMesh;

/// Flat match criteria for the GPU-island scheduler. Built into the core
/// `MatchCriteria` internally (capability composite + numeric filter +
/// selection policy).
#[napi(object)]
pub struct GpuIslandCriteria {
    /// Capability tags every candidate host must carry (AND).
    pub tags_all: Vec<String>,
    /// Minimum GPUs in the island. Omit / 0 = any.
    pub min_gpus: Option<u32>,
    /// Maximum live load (0.0..=1.0). Omit = any.
    pub max_load: Option<f64>,
    /// Maximum live p50 latency (µs). Omit = any.
    pub max_p50_latency_us: Option<u32>,
    /// Require this model already warm in GPU memory. Omit = any.
    pub require_warm_model: Option<BigInt>,
    /// Selection policy: `least_loaded` (default) / `pack` / `load_band`
    /// / `lowest_id`.
    pub selection: Option<String>,
    /// Target load for the `load_band` policy (0.0..=1.0). Ignored by
    /// the other policies.
    pub load_band_target: Option<f64>,
    /// Soft warm-model affinity: islands with this model resident rank
    /// ahead. Omit = none.
    pub prefer_warm_model: Option<BigInt>,
}

/// One island a node self-publishes. Its `host` is forced to this node.
#[napi(object)]
pub struct IslandTopologyInput {
    /// `hash(host, nvlink_domain)` — the island id / reservation key.
    pub id: BigInt,
    /// GPU indices in the NVLink domain.
    pub gpus: Vec<u32>,
    /// Models currently resident in GPU memory.
    pub warm_models: Vec<BigInt>,
    /// Live utilization, 0.0..=1.0.
    pub load: f64,
    /// Live p50 request latency (µs).
    pub p50_latency_us: u32,
}

fn opt_bigint_u64(b: Option<BigInt>) -> Result<Option<u64>> {
    b.map(bigint_u64).transpose()
}

fn build_match_criteria(c: GpuIslandCriteria) -> Result<MatchCriteria> {
    let selection = match c.selection.as_deref() {
        None | Some("least_loaded") => SelectionPolicy::LeastLoaded,
        Some("pack") => SelectionPolicy::Pack,
        Some("lowest_id") => SelectionPolicy::LowestId,
        Some("load_band") => SelectionPolicy::LoadBand(c.load_band_target.unwrap_or(0.5) as f32),
        Some(other) => {
            return Err(Error::from_reason(format!(
                "gang: unknown selection policy {other:?}"
            )))
        }
    };
    Ok(MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: c.tags_all,
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_gpus: c.min_gpus.unwrap_or(0) as usize,
            max_load: c.max_load.map(|v| v as f32),
            max_p50_latency_us: c.max_p50_latency_us,
            require_warm_model: opt_bigint_u64(c.require_warm_model)?,
        },
        selection,
        prefer_warm_model: opt_bigint_u64(c.prefer_warm_model)?,
    })
}

fn claim_outcome_str(o: ClaimOutcome) -> &'static str {
    match o {
        ClaimOutcome::Won => "won",
        ClaimOutcome::Lost => "lost",
    }
}

#[cfg(any(feature = "compute", feature = "cortex", feature = "aggregator"))]
#[napi]
impl NetMesh {
    /// Publish this node's island-topology record (host forced to self).
    /// Self-indexed locally + broadcast to peers; returns the peer
    /// fan-out count.
    #[napi]
    pub async fn publish_island_topology(&self, island: IslandTopologyInput) -> Result<u32> {
        let node = self.node_arc_clone()?;
        let warm_models = island
            .warm_models
            .into_iter()
            .map(bigint_u64)
            .collect::<Result<Vec<_>>>()?;
        let record = IslandRecord {
            id: bigint_u64(island.id)?,
            gpus: GpuSet::new(island.gpus),
            host: 0, // forced to this node by publish
            warm_models,
            load: island.load as f32,
            p50_latency_us: island.p50_latency_us,
        };
        node.publish_island_topology(record)
            .await
            .map(|n| n as u32)
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))
    }

    /// Match GPU islands against `criteria` over this node's folds
    /// (read-only; no claim). Best island first, by id.
    #[napi]
    pub fn match_gpu_islands(&self, criteria: GpuIslandCriteria) -> Result<Vec<BigInt>> {
        let node = self.node_arc_clone()?;
        let mc = build_match_criteria(criteria)?;
        Ok(node
            .match_gpu_islands(&mc)
            .into_iter()
            .map(BigInt::from)
            .collect())
    }

    /// Reserve `island` (optimistic AP CAS) until `untilUnixUs`
    /// (wall-clock micros). Returns `won` / `lost`.
    #[napi]
    pub async fn reserve_island(&self, island: BigInt, until_unix_us: BigInt) -> Result<String> {
        let node = self.node_arc_clone()?;
        let outcome = node
            .reserve_island(bigint_u64(island)?, bigint_u64(until_unix_us)?)
            .await
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))?;
        Ok(claim_outcome_str(outcome).to_string())
    }

    /// Release `island` this node holds. Returns `won` / `lost`
    /// (`lost` if this node wasn't the holder).
    #[napi]
    pub async fn release_island(&self, island: BigInt) -> Result<String> {
        let node = self.node_arc_clone()?;
        let outcome = node
            .release_island(bigint_u64(island)?)
            .await
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))?;
        Ok(claim_outcome_str(outcome).to_string())
    }

    /// Match + reserve the first available island in one call. Returns
    /// its id, or `null` when nothing matched / all contended.
    #[napi]
    pub async fn claim_gpu_island(
        &self,
        criteria: GpuIslandCriteria,
        until_unix_us: BigInt,
    ) -> Result<Option<BigInt>> {
        let node = self.node_arc_clone()?;
        let mc = build_match_criteria(criteria)?;
        let until = bigint_u64(until_unix_us)?;
        node.claim_gpu_island(&mc, until)
            .await
            .map(|o| o.map(BigInt::from))
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))
    }
}
