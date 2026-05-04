//! CortEX adapter — the seam between CortEX events and RedEX storage.
//!
//! Takes a CortEX `EventEnvelope`, projects it into a fixed 20-byte
//! [`EventMeta`] prefix plus a type-specific payload tail, appends the
//! concatenation to a [`super::redex::RedexFile`], and drives a
//! caller-supplied [`super::redex::RedexFold`] as the tail advances.
//! Exposes the materialized state as the read-side NetDB handle.
//!
//! See `docs/CORTEX_ADAPTER_PLAN.md` for the full design.
//!
//! # Layering
//!
//! - **Net** moves events and runs daemons.
//! - **RedEX** keeps a per-node append-only log.
//! - **CortEX adapter** (this module) projects Net events → RedEX
//!   payloads, folds them into local state, exposes that state as a
//!   read handle.
//! - **CortEX / NetDB** (outside this crate) query that state.

mod adapter;
mod config;
mod envelope;
mod error;
mod meta;
#[cfg(feature = "cortex")]
mod watermark;

#[cfg(feature = "cortex")]
pub mod memories;
#[cfg(feature = "cortex")]
pub mod tasks;

pub use adapter::{ChangeEvent, CortexAdapter};
pub use config::{CortexAdapterConfig, FoldErrorPolicy, StartPosition};
pub use envelope::{EventEnvelope, IntoRedexPayload};
pub use error::CortexAdapterError;
pub use meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, DISPATCH_RAW, EVENT_META_SIZE,
    FLAG_CAUSAL, FLAG_CONTINUITY_PROOF,
};
