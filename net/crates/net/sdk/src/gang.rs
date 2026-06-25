//! Gang-claim GPU-island scheduler ("Thunderdome").
//!
//! The peer-aware surface for contended GPU-island arbitration: a node
//! publishes its island topology, matches islands by capability + live
//! numeric axes (load / p50 latency), and reserves/claims them through
//! the mesh's reservation fold.
//!
//! The **live operations** hang off [`Mesh`](crate::mesh::Mesh) — they
//! need a connected node:
//!
//! - [`Mesh::publish_island_topology`](crate::mesh::Mesh::publish_island_topology)
//! - [`Mesh::match_gpu_islands`](crate::mesh::Mesh::match_gpu_islands)
//! - [`Mesh::reserve_island`](crate::mesh::Mesh::reserve_island)
//! - [`Mesh::release_island`](crate::mesh::Mesh::release_island)
//! - [`Mesh::claim_gpu_island`](crate::mesh::Mesh::claim_gpu_island)
//!
//! This module re-exports the **value types** those methods take and
//! return. See [`crate::cortex::workflow`] for the task-lifecycle layer
//! that runs on top of a held island.
//!
//! ## Note on convergence
//!
//! `match_gpu_islands` / `claim_gpu_island` read this node's local
//! capability + island folds. A node sees its *own* announced
//! capabilities and published islands immediately; peer-hosted islands
//! are visible only after their announcements converge over the mesh.
//! On an isolated node, only self-hosted islands match.
//!
//! ## Example
//!
//! ```no_run
//! use net_sdk::gang::{CapabilityFilter, CapabilityQuery, MatchCriteria, NumericFilter, SelectionPolicy};
//!
//! let criteria = MatchCriteria {
//!     capability: CapabilityQuery::Composite(CapabilityFilter {
//!         tags_all: vec!["gpu:h100".into()],
//!         ..Default::default()
//!     }),
//!     numeric: NumericFilter { min_gpus: 8, ..Default::default() },
//!     selection: SelectionPolicy::LeastLoaded,
//!     prefer_warm_model: None,
//! };
//! # let _ = criteria;
//! ```

pub use ::net::adapter::net::behavior::gang::{
    ClaimError, ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};

pub use ::net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityQuery, GpuId, GpuSet, IslandId, IslandRecord, ModelId,
};
