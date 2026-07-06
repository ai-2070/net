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
// Local consent surface — capability identity, the credential-status
// vocabulary, and the allowlist/pin consent gate + persistent pin
// store. Graduated from the MCP bridge adapter (MCP_BRIDGE_SDK_PLAN.md
// P0) because consent isn't MCP-specific: every surface that exposes
// mesh capabilities to a model-driven caller gates on the same local
// decision, and the pin-store lock protocol must have exactly one
// implementation. Unconditional (no feature gate): pure local-state
// primitives with no mesh dependency.
pub mod consent;
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
// Persistent pin store — the machine-shared, lock-protected consent
// records behind `consent`. See the module docs for the two rules
// (no model self-approval; cross-process lock on every mutation).
pub mod pins;
#[cfg(feature = "redis")]
pub mod redis_dedup;
pub mod stream;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "tool")]
pub mod tool;
// On-demand cross-peer movement primitives (blob + directory transfer
// over the fairscheduler stream transport). Needs the networked node
// (`net`) and the blob storage layer (`dataforts`); the transfer
// surface lives here rather than in `dataforts` (which is storage +
// operator read side).
#[cfg(all(feature = "net", feature = "dataforts"))]
pub mod transport;

/// Procedural-macro re-exports gated by the `macros` feature.
///
/// Currently ships the `#[tool]` attribute macro — see the
/// per-macro docs for the full attribute surface and an example.
/// The macro generates a sibling `<fn>_descriptor()` /
/// `<fn>_register(mesh)` pair atop `metadata_for::<Req, Resp>(name)
/// .build()` + `mesh.serve_tool(...)`.
///
/// Always pulled in alongside `tool` because the macro's expansion
/// references `net_sdk::tool::*`; users that don't want the
/// proc-macro2 / syn / quote build cost simply omit the `macros`
/// feature.
#[cfg(feature = "macros")]
pub mod macros {
    pub use net_sdk_macros::tool;
}

#[cfg(feature = "redis")]
pub use redis_dedup::RedisStreamDedup;

// Security surface — identity (keypairs + tokens), capabilities
// (declare + query), and subnets (visibility partitioning). All
// three ride the `net` feature because they share a subprotocol
// dispatch and operate as a single unit at runtime.
#[cfg(feature = "net")]
pub mod capabilities;
// Delegated agent identity — `root → machine → gateway → subagent`
// delegation chains for capability-invocation attribution (Hermes plan
// Phase 3). Rides `net` because it composes the identity / token-chain
// surface; adds only the derivation + verification *convention* over the
// core `TokenChain` / `RevocationRegistry` machinery.
#[cfg(feature = "net")]
pub mod delegation;
// Device enrollment — the invite → join → approve handshake that admits a new
// device into an operator's mesh with a `root → device` delegation to the
// device's *own* key (Hermes V2 Phase 1). Rides `net`: composes the identity /
// delegation surface and pulls `getrandom` for the single-use invite nonce.
#[cfg(feature = "net")]
pub mod enrollment;
// Machine-shared device registry — the operator's inventory of enrolled devices
// (Hermes V2 Phase 1), backing `mesh.devices()`. Inventory/display state, not
// enforcement (that's `revocation`); mirrors the revocation store's file
// discipline. Rides `net` (records `EntityId`s).
#[cfg(feature = "net")]
pub mod devices;
// Operator-side mesh management (Hermes V2 Phase 1) — the transport-independent
// `mesh.invite/approve/revoke/devices` surface, composing the enrollment
// authority + device registry + revocation store into one coordinator.
#[cfg(feature = "net")]
pub mod operator;
// Persistent, machine-shared delegation-revocation floors (Hermes plan Phase
// 3): the provider side of revocation — a running `net wrap` honors an
// operator's revocation of a delegated gateway without a restart. Mirrors the
// pin store's file discipline; composes with a future mesh-published layer.
/// Gang-claim resource-island scheduler value types (live ops on [`mesh::Mesh`]).
#[cfg(feature = "net")]
pub mod gang;
#[cfg(feature = "net")]
pub mod identity;
#[cfg(feature = "net")]
pub mod revocation;
#[cfg(feature = "net")]
pub mod subnets;

// Aggregator + lifecycle surfaces. Aggregator-daemon clients
// (`RegistryClient`, `FoldQueryClient`) + the daemon-author
// types (`AggregatorRegistry`, `LifecycleGroup`,
// `HealthMonitor`). The `aggregator` feature is on by default and
// transitively activates `net` (every surface in this module
// routes through `Mesh::call` / `Mesh::serve_rpc`); the explicit
// flag mirrors the per-binding `aggregator` Cargo feature so a
// `--no-default-features` consumer can opt in by name.
#[cfg(feature = "aggregator")]
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

// Raw substrate handle. The SDK's `Mesh` is the ergonomic
// front-door, but typed RPC clients (`RegistryClient`,
// `FoldQueryClient`) and FFI bindings want the underlying
// `Arc<MeshNode>` directly — `Mesh::node_arc()` hands one out.
// Re-exporting here lets downstream consumers (the CLI's
// remote-attach context, in particular) name the type without
// reaching into `::net::adapter::net` themselves.
#[cfg(feature = "net")]
pub use ::net::adapter::net::MeshNode;

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
// Delegated-identity convenience re-exports (Phase 3): the chain type, the
// shared revocation registry, and the child-seed KDF.
#[cfg(feature = "net")]
pub use crate::delegation::{
    derive_child_seed, DelegationChain, RevocationRegistry, TokenChain, GATEWAY_DELEGATION_CHANNEL,
};
// Device-enrollment convenience re-exports (V2 Phase 1): the invite/join/approve
// types + the displayed root fingerprint.
#[cfg(feature = "net")]
pub use crate::enrollment::{
    fingerprint, Enrollment, EnrollmentAuthority, EnrollmentError, InviteToken, JoinRequest,
};
// Device-inventory convenience re-exports (V2 Phase 1): the registry backing
// `mesh.devices()`.
#[cfg(feature = "net")]
pub use crate::devices::{default_device_registry_path, DeviceRecord, DeviceRegistry};
// Operator-surface convenience re-exports (V2 Phase 1).
#[cfg(feature = "net")]
pub use crate::operator::{OperatorEnrollment, OperatorError};
#[cfg(feature = "net")]
pub use crate::revocation::{default_revocation_store_path, RevocationStore};
#[cfg(feature = "net")]
pub use crate::subnets::{SubnetId, SubnetPolicy};

impl NetBuilder {
    /// Build and start the node.
    pub async fn build(self) -> error::Result<Net> {
        Net::from_builder(self).await
    }
}
