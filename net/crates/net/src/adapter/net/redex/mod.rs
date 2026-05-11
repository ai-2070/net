//! RedEX — local append-only streaming log.
//!
//! A `RedexFile` is a named monotonic log whose on-disk/index entries are
//! 20 bytes each. Payloads live inline (≤8 bytes) or in a heap/disk
//! segment. v1 is strictly local — no replication. Files map 1:1 to
//! [`ChannelName`](super::ChannelName) so the existing
//! [`AuthGuard`](super::AuthGuard) surface applies.
//!
//! See `docs/REDEX_PLAN.md` for the full design.
//!
//! The [`replication`] submodule houses the Phase A wire-protocol scaffold
//! for cross-node replication per `docs/plans/REDEX_DISTRIBUTED_PLAN.md`.
//! Codec layer only — the `ReplicationCoordinator`, heartbeat loop, and
//! election integration land in later phases.

mod config;
#[cfg(feature = "redex-disk")]
mod disk;
mod entry;
mod error;
mod event;
mod file;
mod fold;
mod index;
mod manager;
mod ordered;
pub mod replication;
mod replication_config;
mod retention;
mod segment;
mod typed;

pub use config::{FsyncPolicy, RedexFileConfig};
pub use entry::{RedexEntry, RedexFlags, REDEX_ENTRY_SIZE};
pub use error::RedexError;
pub use event::RedexEvent;
pub use file::RedexFile;
pub use fold::RedexFold;
pub use index::{IndexOp, IndexStart, RedexIndex};
pub use manager::Redex;
pub use ordered::OrderedAppender;
pub use replication::{
    ChannelId, ReplicaRole, SyncEvent, SyncHeartbeat, SyncNack, SyncNackError, SyncRequest,
    SyncResponse, WireError as ReplicationWireError, DISPATCH_REPLICA_SYNC_RESERVED_END,
    DISPATCH_SYNC_HEARTBEAT, DISPATCH_SYNC_NACK, DISPATCH_SYNC_REQUEST, DISPATCH_SYNC_RESPONSE,
    SUBPROTOCOL_REDEX, SYNC_HEARTBEAT_SIZE, SYNC_NACK_DETAIL_MAX, SYNC_REQUEST_SIZE,
};
pub use replication_config::{
    PlacementStrategy, ReplicationConfig, ReplicationConfigError, UnderCapacity,
    HEARTBEAT_MS_DEFAULT, HEARTBEAT_MS_MIN, REPLICATION_BUDGET_FRACTION_DEFAULT,
    REPLICATION_FACTOR_DEFAULT, REPLICATION_FACTOR_MAX, REPLICATION_FACTOR_MIN,
};
pub use typed::TypedRedexFile;
