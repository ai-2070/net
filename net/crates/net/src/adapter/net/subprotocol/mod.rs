//! Layer 6: Subprotocols for Net.
//!
//! Formalizes the `subprotocol_id: u16` field in the Net header. Provides
//! a registry mapping IDs to handler metadata, version negotiation between
//! peers, and capability-aware routing via tags.
//!
//! Opaque forwarding is the default — nodes that don't understand a
//! subprotocol forward packets without inspecting the payload.

mod descriptor;
pub mod migration_handler;
mod negotiation;
mod registry;
// `stream_window` moved to `net-mesh-wire` in Stage 5 (plan §7: the
// wire-level subprotocol codecs belong to the wire crate; S0a
// deferred the move until the leaf dispatcher needed it). Re-exported
// under its previous path so no `use
// …::subprotocol::stream_window::…` site changed.
pub use net_wire::stream_window;

/// Subprotocol ID for negotiation messages.
pub const SUBPROTOCOL_NEGOTIATION: u16 = 0x0600;

pub use descriptor::{SubprotocolDescriptor, SubprotocolVersion, MANIFEST_ENTRY_SIZE};
pub use migration_handler::{
    EnvelopeUnsealFn, FailureCallback, MigrationHandlerHooks, MigrationIdentityContext,
    MigrationOrchestratorPolicy, MigrationSubprotocolHandler, OutboundMigrationMessage,
    PostRestoreCallback, PreCleanupCallback, ReadinessCallback,
};
pub use negotiation::{negotiate, ManifestEntry, NegotiatedSet, SubprotocolManifest};
pub use registry::SubprotocolRegistry;
pub use stream_window::{StreamWindow, SUBPROTOCOL_STREAM_WINDOW};
