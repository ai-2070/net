//! Gang-claim resource-island scheduler ("Thunderdome").
//!
//! The peer-aware surface for contended resource-island arbitration: a
//! node publishes its island topology, matches islands by capability +
//! live numeric axes (load / p50 latency), and reserves/claims them
//! through the mesh's reservation fold. An island is a co-located pool
//! of exclusive units — a GPU NVLink domain is the motivating instance,
//! with GPU specifics riding plain capability tags (`gpu:h100`,
//! `model:<hex>`).
//!
//! The **live operations** hang off [`Mesh`](crate::mesh::Mesh) — they
//! need a connected node:
//!
//! - [`Mesh::publish_island_topology`](crate::mesh::Mesh::publish_island_topology)
//! - [`Mesh::match_islands`](crate::mesh::Mesh::match_islands)
//! - [`Mesh::reserve_island`](crate::mesh::Mesh::reserve_island)
//! - [`Mesh::release_island`](crate::mesh::Mesh::release_island)
//! - [`Mesh::claim_island`](crate::mesh::Mesh::claim_island)
//!
//! This module re-exports the **value types** those methods take and
//! return. See [`crate::cortex::workflow`] for the task-lifecycle layer
//! that runs on top of a held island.
//!
//! ## Note on convergence
//!
//! `match_islands` / `claim_island` read this node's local capability +
//! island folds. A node sees its *own* announced capabilities and
//! published islands immediately; peer-hosted islands are visible only
//! after their announcements converge over the mesh. On an isolated
//! node, only self-hosted islands match.
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
//!     numeric: NumericFilter { min_units: 8, ..Default::default() },
//!     selection: SelectionPolicy::LeastLoaded,
//!     prefer_capability: None,
//! };
//! # let _ = criteria;
//! ```

pub use ::net::adapter::net::behavior::gang::{
    ClaimError, ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};

pub use ::net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityQuery, IslandId, IslandRecord, UnitId, UnitSet,
};
