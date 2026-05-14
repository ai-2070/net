//! Deck SDK — operator-side surface.
//!
//! Customer-facing entry point for the cyberdeck operator
//! workflows (admin commits, snapshot subscription, audit
//! queries, log streams, ICE). The implementation lives in the
//! substrate at `net::adapter::net::behavior::deck`; this
//! module re-exports the types under a clean `net_sdk::deck::*`
//! path so consumers don't reach into substrate internals.
//!
//! # Surface
//!
//! - [`DeckClient`] — composes a live MeshOS runtime with an
//!   [`OperatorIdentity`] into the operator-facing handle.
//! - [`AdminCommands`] — typed admin-event surface. One method
//!   per [`AdminEvent`] variant; each returns a [`ChainCommit`]
//!   correlation handle.
//! - [`IceCommands`] / [`IceProposal`] — break-glass surface.
//!   Each proposal exposes `simulate()` → [`BlastRadius`] and
//!   `commit(signatures: &[OperatorSignature])` with substrate-
//!   side M-of-N verification via [`AdminVerifier`].
//! - [`SnapshotStream`] / [`StatusSummaryStream`] — `Stream`s
//!   over the runtime's snapshot reader.
//! - [`AuditQuery`] — fluent admin-chain query builder over the
//!   in-memory audit ring (with the
//!   `<<deck-sdk-kind:KIND>>MSG` error discriminator). Filters:
//!   `recent`, `by_operator`, `between`, `force_only`, `since`.
//! - [`LogStream`] / [`FailureStream`] — per-daemon / per-node
//!   log + failure tails with `since(seq)` pagination.
//! - [`DeckError`] / [`AdminError`] / [`IceError`] — operator-
//!   readable error surface.
//!
//! # Operator-side, not daemon-side
//!
//! Daemons author against [`crate::meshos::MeshOsDaemonSdk`];
//! operators command against [`DeckClient`]. The two surfaces
//! share the type shapes ([`MeshOsSnapshot`], the
//! `<<…-sdk-kind:KIND>>MSG` error discriminator) without
//! sharing the action surface.

// Re-export the substrate-side Deck types under a clean
// `net_sdk::deck::*` path.
pub use net::adapter::net::behavior::deck::{
    AdminCommands, AdminError, AuditQuery, AuditStream, ChainCommit, DaemonCounts, DeckClient,
    DeckClientConfig, DeckError, FailureStream, IceCommands, IceError, IceProposal, LogFilter,
    LogStream, OperatorIdentity, OperatorRegistry, OperatorSignature, PeerCounts, SnapshotStream,
    StatusSummary, StatusSummaryStream,
};

// Supporting types operators need from the MeshOS surface to
// build commands or read snapshots.
pub use net::adapter::net::behavior::meshos::{
    AdminAuditRecord, AdminEvent, AdminVerifier, AvoidScope, BlastRadius, BlastWarning, ChainId,
    DaemonHealthSnapshot, DaemonLifecycleSnapshot, DaemonSnapshot, FailureRecord,
    IceActionProposal, LogLevel, LogLine, LogRecord, MaintenanceStateSnapshot, MeshOsSnapshot,
    MigrationId, MigrationPhaseSnapshot, MigrationSnapshot, NodeId, PeerHealthSnapshot,
    PeerSnapshot, ReplicaSnapshot, RestartStateSnapshot, VerificationOutcome, VerifyError,
};
