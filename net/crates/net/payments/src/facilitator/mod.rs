//! Facilitator clients: the verify/settle boundary.
//!
//! - [`traits`] — the interface (P0 and P1 share it; that's the design's
//!   acceptance test).
//! - [`mock`] — the in-process P0 facilitator with injectable behaviors;
//!   the conformance simulator every real network passes before real
//!   money exists.
//! - [`client`] — the real-facilitator HTTP client (P1; config, not code).
//! - [`packs`] — well-known network config packs for the P1 survey
//!   networks (data-only constructors; the "config, not code" proof).

pub mod client;
pub mod config;
pub mod mock;
pub mod packs;
pub mod traits;

pub use traits::{
    Facilitator, FacilitatorError, FacilitatorErrorKind, SettleOutcome, VerifyOutcome,
};
