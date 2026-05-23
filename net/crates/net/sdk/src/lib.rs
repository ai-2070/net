//! # Net SDK
//!
//! Ergonomic Rust SDK for the Net mesh network.
//!
//! The core `net` crate is the engine. This SDK is what developers actually import.
//!
//! # Example
//!
//! ```rust,no_run
//! use net_sdk::{Net, Backpressure};
//! use futures::StreamExt;
//!
//! # async fn example() -> net_sdk::error::Result<()> {
//! let node = Net::builder()
//!     .shards(4)
//!     .backpressure(Backpressure::DropOldest)
//!     .memory()
//!     .build()
//!     .await?;
//!
//! // Emit events
//! node.emit(&serde_json::json!({"token": "hello"}))?;
//! node.emit_raw(b"{\"token\": \"world\"}" as &[u8])?;
//!
//! // Subscribe to a stream
//! let mut stream = node.subscribe(Default::default());
//! while let Some(event) = stream.next().await {
//!     let event = event?;
//!     println!("{}", event.raw_str().unwrap_or("<non-utf8>"));
//! }
//!
//! node.shutdown().await?;
//! # Ok(())
//! # }
//! ```

/// Crate version, sourced from `Cargo.toml` at build time. Re-
/// exported so downstream binaries (the `net` CLI in particular)
/// can report the embedded SDK version without hardcoding a
/// literal that silently drifts on every workspace bump.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(feature = "compute")]
pub mod compute;
pub mod config;
#[cfg(feature = "cortex")]
pub mod cortex;
#[cfg(feature = "dataforts")]
pub mod dataforts;
#[cfg(feature = "deck")]
pub mod deck;
pub mod error;
#[cfg(feature = "groups")]
pub mod groups;
#[cfg(feature = "net")]
pub mod mesh;
#[cfg(all(feature = "net", feature = "cortex"))]
pub mod mesh_rpc;
#[cfg(all(feature = "net", feature = "cortex"))]
pub mod mesh_rpc_resilience;
#[cfg(feature = "meshdb")]
pub mod meshdb;
#[cfg(feature = "meshos")]
pub mod meshos;
mod net;
#[cfg(feature = "redis")]
pub mod redis_dedup;
pub mod stream;
#[cfg(feature = "testing")]
pub mod testing;

#[cfg(feature = "redis")]
pub use redis_dedup::RedisStreamDedup;

// Security surface — identity (keypairs + tokens), capabilities
// (declare + query), and subnets (visibility partitioning). All
// three ride the `net` feature because they share a subprotocol
// dispatch and operate as a single unit at runtime.
#[cfg(feature = "net")]
pub mod capabilities;
#[cfg(feature = "net")]
pub mod identity;
#[cfg(feature = "net")]
pub mod subnets;

// Aggregator + lifecycle surfaces. Aggregator-daemon clients
// (`RegistryClient`, `FoldQueryClient`) + the daemon-author
// types (`AggregatorRegistry`, `LifecycleGroup`,
// `HealthMonitor`). Riding `net` because every surface in this
// module routes through `Mesh::call` / `Mesh::serve_rpc`.
#[cfg(feature = "net")]
pub mod aggregator;

// Re-export the main handle.
pub use crate::net::{Net, PollRequest, PollResponse, Receipt, Stats};

// Re-export config types.
pub use crate::config::{Backpressure, NetBuilder};

// Re-export stream types.
pub use crate::stream::{EventStream, SubscribeOpts, TypedEventStream};

// Re-export core types that users will need.
pub use ::net::config::{BatchConfig, ScalingPolicy};
pub use ::net::consumer::Ordering;
pub use ::net::event::{Event, RawEvent, StoredEvent};
pub use ::net::Filter;

// Feature-gated re-exports.
#[cfg(feature = "redis")]
pub use ::net::config::RedisAdapterConfig;

#[cfg(feature = "jetstream")]
pub use ::net::config::JetStreamAdapterConfig;

#[cfg(feature = "net")]
pub use ::net::adapter::net::NetAdapterConfig;

#[cfg(feature = "net")]
pub use ::net::adapter::net::{
    CloseBehavior, Reliability, Stream as MeshStream, StreamConfig, StreamStats,
};

// Channel (distributed pub/sub) types. Ship alongside `net` because
// they live on the mesh transport — subscribing / publishing require
// a live `Mesh`.
#[cfg(feature = "net")]
pub use ::net::adapter::net::{
    AckReason, ChannelConfig, ChannelId, ChannelName, OnFailure, PublishConfig, PublishReport,
    Visibility,
};

#[cfg(feature = "net")]
pub use crate::mesh::{Mesh, MeshBuilder, SubscribeOptions};

// Compute surface — `MeshDaemon` trait + runtime. Gated by the
// `compute` feature (which depends on `net`).
#[cfg(feature = "compute")]
pub use crate::compute::{
    CausalEvent, CausalLink, DaemonError as ComputeDaemonError, DaemonHandle, DaemonHostConfig,
    DaemonRuntime, DaemonStats, MeshDaemon, MigrationError, MigrationHandle, MigrationOpts,
    MigrationPhase, StateSnapshot,
};

// Convenience re-exports for the common security types, so users can
// `use net_sdk::{Identity, TokenScope};` without reaching for a
// sub-module path.
#[cfg(feature = "net")]
pub use crate::capabilities::{CapabilityFilter, CapabilitySet};
#[cfg(feature = "net")]
pub use crate::identity::{Identity, PermissionToken, TokenError, TokenScope};
#[cfg(feature = "net")]
pub use crate::subnets::{SubnetId, SubnetPolicy};

impl NetBuilder {
    /// Build and start the node.
    pub async fn build(self) -> error::Result<Net> {
        Net::from_builder(self).await
    }
}
