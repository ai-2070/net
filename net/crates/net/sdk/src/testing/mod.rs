//! Testing scaffolding — in-process multi-node `MeshOsRuntime`
//! harness + bridge probes that wire a real `Mesh` into a real
//! `MeshOsRuntime`'s probe registry.
//!
//! Gated behind the `testing` Cargo feature; consumers opt in via
//! `[dev-dependencies] ai2070-net-sdk = { ..., features = ["testing"] }`.
//! Never linked into a release binary.
//!
//! # What's here
//!
//! - [`ClusterHarness`] — boots N `(Mesh, MeshOsDaemonSdk)` pairs
//!   on `127.0.0.1:<ephemeral>`, peers every Mesh pair via real
//!   UDP handshake, and installs bridge probes so the MeshOS
//!   snapshot fold reflects peer state.
//! - [`MeshLocalityProbe`] / [`MeshHealthProbe`] /
//!   [`MeshInventoryProbe`] — `LocalityProbe` / `HealthProbe` /
//!   `InventoryProbe` impls that read peer state off a `Mesh`.
//!   The bridge that closes the substrate's two-layer split:
//!   network on one side, MeshOS state fold on the other.
//!
//! # Why this exists
//!
//! See `crates/net/docs/plans/DECK_DEMO_HARNESS_PLAN.md` Phase 0 +
//! Phase 0.5. The short version: `MeshOsRuntime` is a pure
//! in-memory snapshot fold (sdk.rs:643 constructs it with an empty
//! `ProbeRegistry` and no network bind), and `Mesh` is the
//! network. They communicate through probe traits the runtime
//! polls each tick. Without bridge probes, a multi-`MeshOsRuntime`
//! cluster has fully-peered Meshes underneath but every runtime's
//! `snapshot.peers` stays empty.

pub mod cluster;
pub mod probes;

pub use cluster::{
    ClusterConfig, ClusterError, ClusterHarness, ClusterHealth, ClusterNode, NodeDaemonHandle,
};
pub use probes::{install_mesh_probes, MeshHealthProbe, MeshInventoryProbe, MeshLocalityProbe};
