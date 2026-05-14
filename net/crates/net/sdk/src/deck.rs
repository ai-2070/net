//! Deck SDK — operator-side surface.
//!
//! Customer-facing entry point for the cyberdeck operator
//! workflows (admin commits, snapshot subscription, audit
//! queries, log streams, ICE). The implementation lives in the
//! substrate at `net::adapter::net::behavior::deck`; this
//! module re-exports the types under a clean `net_sdk::deck::*`
//! path so consumers don't reach into substrate internals.
//!
//! # Surface (Phase 1)
//!
//! - [`DeckClient`] — composes a live MeshOS runtime with an
//!   [`OperatorIdentity`] into the operator-facing handle.
//! - [`AdminCommands`] — typed admin-event surface. One method
//!   per [`AdminEvent`] variant; each returns a [`ChainCommit`]
//!   correlation handle.
//! - [`SnapshotStream`] — `Stream` over the runtime's snapshot
//!   reader.
//! - [`DeckError`] / [`AdminError`] — operator-readable error
//!   surface with the `<<deck-sdk-kind:KIND>>MSG` discriminator
//!   format every cross-language SDK uses.
//!
//! # Deferred to later slices
//!
//! - `audit()` — admin-chain query surface. Needs the
//!   substrate's signed admin chain to query against.
//! - `subscribe_logs()` — per-daemon / per-node log streams.
//! - `ice()` — break-glass surface (Phase 3). Depends on the
//!   substrate's `AdminEvent::Force*` variants + multi-operator
//!   signing + blast-radius simulator (Phase 2 substrate work).
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
    AdminCommands, AdminError, AuditQuery, ChainCommit, DaemonCounts, DeckClient,
    DeckClientConfig, DeckError, IceCommands, IceError, IceProposal, OperatorIdentity,
    OperatorRegistry, OperatorSignature, PeerCounts, SnapshotStream, StatusSummary,
};

// Supporting types operators need from the MeshOS surface to
// build commands or read snapshots.
pub use net::adapter::net::behavior::meshos::{
    AdminAuditRecord, AdminEvent, AdminVerifier, AvoidScope, BlastRadius, BlastWarning, ChainId,
    DaemonHealthSnapshot, DaemonLifecycleSnapshot, DaemonSnapshot, IceActionProposal,
    MaintenanceStateSnapshot, MeshOsSnapshot, NodeId, PeerHealthSnapshot, PeerSnapshot,
    ReplicaSnapshot, RestartStateSnapshot, VerificationOutcome, VerifyError,
};
