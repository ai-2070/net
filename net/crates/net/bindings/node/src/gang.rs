//! napi bindings for the gang-claim resource-island scheduler — methods
//! on the `NetMesh` class plus the flat criteria / island-input objects.
//!
//! The core `MatchCriteria` embeds a rich `CapabilityQuery` enum; the JS
//! boundary instead takes a flat [`IslandCriteria`] and builds the
//! composite capability query + numeric filter + selection policy
//! internally, so callers never touch the internal enum shapes. The
//! scheduler is resource-agnostic — GPU specifics ride plain capability
//! tags (`gpu:h100`, `model:<hex>`).

use ::net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityQuery, IslandRecord, UnitSet,
};
use ::net::adapter::net::behavior::gang::{
    ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::common::bigint_u64;
use crate::NetMesh;

/// Flat match criteria for the island scheduler. Built into the core
/// `MatchCriteria` internally (capability composite + numeric filter +
/// selection policy).
#[napi(object)]
pub struct IslandCriteria {
    /// Host capability tags every candidate must carry (AND).
    pub tags_all: Vec<String>,
    /// Host must carry at least one of these tags (OR). Omit = any.
    pub tags_any: Option<Vec<String>>,
    /// Host must carry at least one tag from *every* group (AND of ORs).
    /// Omit = any.
    pub tag_groups_all: Option<Vec<Vec<String>>>,
    /// Host network-locality — subnet / zone / availability region.
    /// Omit = any region.
    pub region: Option<String>,
    /// Minimum exclusive units in the island. Omit / 0 = any.
    pub min_units: Option<u32>,
    /// Maximum live load (0.0..=1.0). Omit = any.
    pub max_load: Option<f64>,
    /// Maximum live p50 latency (µs). Omit = any.
    pub max_p50_latency_us: Option<u32>,
    /// Resident capabilities the island must have ALL of (AND) — e.g.
    /// `model:<hex>` for a warm model. Omit / empty = any.
    pub require_all: Option<Vec<String>>,
    /// Resident capabilities the island must have AT LEAST ONE of (OR).
    /// Omit / empty = any.
    pub require_any: Option<Vec<String>>,
    /// Selection policy: `least_loaded` (default) / `pack` / `load_band`
    /// / `lowest_id`.
    pub selection: Option<String>,
    /// Target load for the `load_band` policy (0.0..=1.0). Ignored by
    /// the other policies.
    pub load_band_target: Option<f64>,
    /// Soft capability affinity: islands with this capability resident
    /// rank ahead. Omit = none.
    pub prefer_capability: Option<String>,
}

/// One island a node self-publishes. Its `host` is forced to this node.
#[napi(object)]
pub struct IslandTopologyInput {
    /// `hash(host, domain)` — the island id / reservation key.
    pub id: BigInt,
    /// The exclusive unit indices composing this island.
    pub units: Vec<u32>,
    /// Capabilities currently resident on this island (e.g.
    /// `model:<hex>` tags).
    pub capabilities: Vec<String>,
    /// Live utilization, 0.0..=1.0.
    pub load: f64,
    /// Live p50 request latency (µs).
    pub p50_latency_us: u32,
}

fn build_match_criteria(c: IslandCriteria) -> Result<MatchCriteria> {
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
            tags_any: c.tags_any.unwrap_or_default(),
            tag_groups_all: c.tag_groups_all.unwrap_or_default(),
            region: c.region,
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: c.min_units.unwrap_or(0) as usize,
            max_load: c.max_load.map(|v| v as f32),
            max_p50_latency_us: c.max_p50_latency_us,
            require_all: c.require_all.unwrap_or_default(),
            require_any: c.require_any.unwrap_or_default(),
        },
        selection,
        prefer_capability: c.prefer_capability,
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
        let record = IslandRecord {
            id: bigint_u64(island.id)?,
            units: UnitSet::new(island.units),
            host: 0, // forced to this node by publish
            capabilities: island.capabilities,
            load: island.load as f32,
            p50_latency_us: island.p50_latency_us,
        };
        node.publish_island_topology(record)
            .await
            .map(|n| n as u32)
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))
    }

    /// Match islands against `criteria` over this node's folds
    /// (read-only; no claim). Best island first, by id.
    #[napi]
    pub fn match_islands(&self, criteria: IslandCriteria) -> Result<Vec<BigInt>> {
        let node = self.node_arc_clone()?;
        let mc = build_match_criteria(criteria)?;
        Ok(node
            .match_islands(&mc)
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
    pub async fn claim_island(
        &self,
        criteria: IslandCriteria,
        until_unix_us: BigInt,
    ) -> Result<Option<BigInt>> {
        let node = self.node_arc_clone()?;
        let mc = build_match_criteria(criteria)?;
        let until = bigint_u64(until_unix_us)?;
        node.claim_island(&mc, until)
            .await
            .map(|o| o.map(BigInt::from))
            .map_err(|e| Error::from_reason(format!("gang: {}", e)))
    }
}
