//! Aggregator daemon scaffolding (Phase B of
//! `SCALING_SUBNET_SPEC.md`).
//!
//! The aggregator role bridges subnet tiers: subscribes to detail
//! channels in a source subnet, rolls them up into summaries, and
//! republishes the summaries onto channels with broader visibility
//! (typically [`Visibility::ParentVisible`] or
//! [`Visibility::Global`]).
//!
//! # Status
//!
//! - This module ships the configuration shape, the [`Summarizer`]
//!   trait, and built-in summarizers for `CapabilityFold` /
//!   `ReservationFold` summaries.
//! - The actual `MeshDaemon` integration is **not yet** wired —
//!   the existing `MeshDaemon` trait is event-processor-shaped
//!   (`process(&CausalEvent) -> Vec<Bytes>`) and doesn't carry the
//!   `on_start` + `Event::Tick` hooks the aggregator design needs.
//!   That extension lands in a follow-up slice.
//!
//! # What ships here
//!
//! - [`AggregatorConfig`] — operator-facing configuration: source
//!   subnet, summary visibility / targets, fold kinds to
//!   aggregate, summary cadence, optional custom summarizers per
//!   fold kind.
//! - [`Summarizer`] — trait the substrate calls once per
//!   summary-interval tick. Implementations read a fold's current
//!   state and produce a slice of summary payloads.
//! - [`SummaryAnnouncement`] — the wire-shaped payload a
//!   summarizer emits. Carries the source-subnet identifier and
//!   the per-bucket counts the aggregator publishes.
//! - Built-in summarizer scaffolds for the capability fold (see
//!   [`summarizer::CapabilityFoldSummarizer`]).
//!
//! See `docs/plans/SCALING_SUBNET_SPEC.md` §"AggregatorDaemon as
//! a MeshDaemon" for the full design.

pub mod config;
pub mod daemon;
pub mod summarizer;

pub use config::AggregatorConfig;
pub use daemon::{AggregatorDaemon, AggregatorError};
pub use summarizer::{
    CapabilityFoldSummarizer, ReservationFoldSummarizer, SummaryAnnouncement, Summarizer,
};
