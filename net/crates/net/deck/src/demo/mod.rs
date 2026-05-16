//! Live multi-node demo runtime. Booted via `cargo run
//! --features demo`. Replaces the synthetic
//! `runtime::samples` fixture with a real in-process cluster
//! of N `(Mesh, MeshOsRuntime, DaemonRuntime)` triples
//! provisioned by `net_sdk::testing::ClusterHarness`.
//!
//! See `crates/net/docs/plans/DECK_DEMO_PLAN.md` for the
//! design rationale and phase breakdown. Phase 1 (this slice)
//! boots the cluster and registers a `HeartbeatDaemon` per
//! node — enough for LOGS / MESH.EVENTS / NODES / NET.MAP /
//! DAEMONS to render real data. Subsequent slices add group-
//! based daemons (replicas / forks / standby), real dataforts
//! activity, real migrations, and real nRPC observation.

pub mod cluster;
pub mod daemons;
pub mod dataforts;
pub mod migrator;
pub mod spawn;

// `Harness` is part of the public demo surface (it's the
// return type of `spawn`) even though `main.rs` doesn't
// import its name directly. Keep the re-export and silence
// the unused-import lint that fires under that consumption
// pattern.
#[allow(unused_imports)]
pub use spawn::{spawn, Harness};
