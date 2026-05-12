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
//! The private `replication` submodule houses the Phase A
//! wire-protocol scaffold for cross-node replication per
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md`. Its public types
//! ([`SyncRequest`], [`SyncResponse`], [`SyncHeartbeat`],
//! [`SyncNack`], [`ChannelId`], [`ReplicaRole`]) are re-exported
//! flat under `redex::`; the codec layer only — the
//! `ReplicationCoordinator`, heartbeat loop, and election
//! integration land in later phases.

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
mod replication;
mod replication_budget;
mod replication_catchup;
mod replication_config;
mod replication_coordinator;
mod replication_election;
mod replication_heartbeat;
mod replication_metrics;
mod replication_router;
mod replication_runtime;
mod replication_state;
mod replication_step;
mod retention;
mod segment;
mod typed;
mod write_token;

pub use config::{FsyncPolicy, RedexFileConfig};
pub use entry::{RedexEntry, RedexFlags, REDEX_ENTRY_SIZE};
pub use error::RedexError;
pub use event::RedexEvent;
pub use file::RedexFile;
pub use fold::RedexFold;
pub use index::{IndexOp, IndexStart, RedexIndex};
pub use manager::{Redex, ReplicationChannelStatus};
pub use ordered::OrderedAppender;
pub use replication::{
    ChannelId, ReplicaRole, SyncEvent, SyncHeartbeat, SyncNack, SyncNackError, SyncRequest,
    SyncResponse, WireError as ReplicationWireError, DISPATCH_REPLICA_SYNC_RESERVED_END,
    DISPATCH_SYNC_HEARTBEAT, DISPATCH_SYNC_NACK, DISPATCH_SYNC_REQUEST, DISPATCH_SYNC_RESPONSE,
    SUBPROTOCOL_REDEX, SYNC_HEARTBEAT_SIZE, SYNC_NACK_DETAIL_MAX, SYNC_REQUEST_SIZE,
};
pub use replication_budget::BandwidthBudget;
pub use replication_catchup::{
    apply_sync_response, handle_sync_request, ApplyError, SyncRequestOutcome,
    CHUNK_MAX_HARD_CEILING_BYTES,
};
pub use replication_config::{
    PlacementStrategy, ReplicationConfig, ReplicationConfigError, UnderCapacity,
    HEARTBEAT_MS_DEFAULT, HEARTBEAT_MS_MAX, HEARTBEAT_MS_MIN, REPLICATION_BUDGET_FRACTION_DEFAULT,
    REPLICATION_FACTOR_DEFAULT, REPLICATION_FACTOR_MAX, REPLICATION_FACTOR_MIN,
};
pub use replication_coordinator::{
    ChainTagSink, ChannelIdentity, CoordinatorError, ReplicationCoordinator,
};
pub use replication_election::{elect, ElectionOutcome};
pub use replication_heartbeat::{HeartbeatTracker, PeerState, DEFAULT_MISS_THRESHOLD};
pub use replication_metrics::{
    ChannelMetrics, ChannelMetricsAtomic, ReplicationMetricsRegistry, ReplicationMetricsSnapshot,
    MAX_TRACKED_CHANNELS, OVERFLOW_CHANNEL_LABEL,
};
pub use replication_router::RedexReplicationRouter;
pub use replication_runtime::{
    spawn_replication_runtime, Inbound, ReplicationDispatcher, ReplicationInboundRouter,
    ReplicationRuntimeHandle, RttLookup, RuntimeInputs, RUNTIME_INBOX_CAPACITY,
};
pub use replication_state::{StateTransition, StateTransitionError, TransitionSignal};
pub use replication_step::{
    election_outcome, tick, OutboundMessage, PendingTransition, StepOutcome, TickInputs,
};
pub use typed::TypedRedexFile;
pub use write_token::{WriteToken, WriteTokenParseError};
