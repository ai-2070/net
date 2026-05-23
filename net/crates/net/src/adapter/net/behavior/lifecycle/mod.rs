//! Async lifecycle daemons — sibling primitive to
//! [`MeshDaemon`](crate::adapter::net::compute::MeshDaemon) for
//! daemons that need an async runtime + start/stop lifecycle
//! hooks the sync, WASM-targeted `MeshDaemon` trait doesn't
//! carry.
//!
//! See [`daemon`] for the trait surface + RAII handle, and
//! [`group`] for the [`LifecycleGroup`] primitive that manages N
//! interchangeable replicas of a single `L: LifecycleDaemon`.
//!
//! # Why a sibling trait, not async `MeshDaemon`?
//!
//! `MeshDaemon::process(&CausalEvent) -> Vec<Bytes>` is
//! documented sync-only / WASM-compatible (see its module doc)
//! and has ~30 impls including the Python / Node / Go FFI
//! bridges. Retrofitting async onto that trait would force every
//! FFI binding to re-bridge sync Rust ↔ language-native async.
//! Splitting the async story into its own trait costs duplication
//! but keeps the WASM-friendly contract intact for daemons that
//! don't need a tokio runtime.

pub mod daemon;
pub mod group;
pub mod monitor;

pub use daemon::{LifecycleDaemon, LifecycleError, LifecycleHandle, ReplicaHealth};
pub use group::{LifecycleGroup, LifecycleGroupError, ReplicaContext};
pub use monitor::{HealthMonitor, HealthMonitorStats};
