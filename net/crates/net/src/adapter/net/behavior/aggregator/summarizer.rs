//! [`Summarizer`] trait + built-in summarizer scaffolds.
//!
//! A `Summarizer` is the per-fold reducer the aggregator daemon
//! invokes once per summary-interval tick. The trait keeps the
//! implementation generic over fold kind so a single
//! `AggregatorDaemon` can carry summarizers for capability /
//! reservation / future fold types side-by-side.

use std::sync::Arc;

use crate::adapter::net::behavior::fold::capability::CapabilityFold;
use crate::adapter::net::behavior::fold::reservation::ReservationFold;
use crate::adapter::net::behavior::fold::{Fold, FoldKind, NodeState};
use crate::adapter::net::subnet::SubnetId;

/// Wire-shaped payload a [`Summarizer`] emits. Carries the
/// source-subnet identifier (so receivers know which subnet's
/// detail a row summarizes), the fold-kind it summarizes, and a
/// flat string→u64 bucket map operator tooling renders.
///
/// Buckets keys are arbitrary fold-specific strings — e.g.
/// `"idle"` / `"busy"` for `CapabilityFold` state, `"class:0xH"`
/// for class-bucketed counts. Receivers diff successive
/// announcements via `(source_subnet, fold_kind, bucket)` to
/// detect deltas.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SummaryAnnouncement {
    /// Subnet this summary describes. Filled in by the aggregator
    /// daemon from its [`super::AggregatorConfig::source_subnet`].
    pub source_subnet: SubnetId,
    /// `FoldKind::KIND_ID` of the fold this summary covers.
    pub fold_kind: u16,
    /// Aggregator's monotonic generation counter. Receivers
    /// dedupe + order announcements on this.
    pub generation: u64,
    /// Bucket name → count. Sorted by bucket name on emission
    /// for stable wire bytes; the field is `Vec<(String, u64)>`
    /// rather than `HashMap` so callers can rely on insertion
    /// order on the wire.
    pub buckets: Vec<(String, u64)>,
}

/// Per-fold reducer the aggregator daemon invokes at every
/// summary-interval tick. Implementations read the fold's
/// current state and produce a `Vec` of summary announcements
/// — typically one per fold-kind, but custom impls may
/// produce per-class / per-region rollups (the substrate
/// republishes each entry independently).
///
/// Bounded by `Send + Sync` so a single summarizer instance can
/// be shared by every replica in a group via `Arc<dyn Summarizer>`.
pub trait Summarizer: Send + Sync {
    /// The fold-kind id this summarizer is registered against.
    /// Used by the aggregator daemon to confirm the registration
    /// matches the requested fold kinds in
    /// [`super::AggregatorConfig::fold_kinds`].
    fn fold_kind(&self) -> u16;

    /// Compute the summary for one tick.
    fn summarize(&self, ctx: &SummarizerContext<'_>) -> Vec<SummaryAnnouncement>;
}

/// Context handed to [`Summarizer::summarize`] at every tick.
/// Hold-by-borrow types so summarizers don't accidentally clone
/// the substrate-level fold (which they read by trait method).
pub struct SummarizerContext<'a> {
    /// Aggregator's source subnet, stamped onto every emitted
    /// announcement.
    pub source_subnet: SubnetId,
    /// Monotonic generation counter the daemon bumps once per
    /// summary tick.
    pub generation: u64,
    /// Type-erased fold handle. Concrete summarizers use the
    /// associated downcast methods on this context.
    pub fold: &'a dyn FoldHandle,
}

/// Type-erased handle the [`SummarizerContext`] carries. The
/// daemon constructs one per (replica, fold-kind) pair and hands
/// a `&dyn FoldHandle` to each summarizer; impls downcast through
/// the typed `capability_fold` / `reservation_fold` getters.
///
/// Trait-object indirection (instead of an enum) keeps the
/// summarizer API extensible — new fold kinds add a method here
/// without touching every existing summarizer impl.
pub trait FoldHandle: Send + Sync {
    /// Concrete capability fold, when this handle was constructed
    /// from one. Returns `None` for other fold kinds.
    fn capability_fold(&self) -> Option<&Fold<CapabilityFold>> {
        None
    }
    /// Concrete reservation fold, when this handle was constructed
    /// from one. Returns `None` for other fold kinds.
    fn reservation_fold(&self) -> Option<&Fold<ReservationFold>> {
        None
    }
}

/// Built-in summarizer for `CapabilityFold`. Emits one
/// announcement carrying per-state counts (`idle`, `busy`,
/// `reserved`, `faulty`) across every entry the fold currently
/// holds.
///
/// Operators that want richer rollups (per-class, per-region,
/// hardware-shape breakdowns) supply a custom impl via
/// `AggregatorConfig::with_custom_summarizer`.
pub struct CapabilityFoldSummarizer;

impl Summarizer for CapabilityFoldSummarizer {
    fn fold_kind(&self) -> u16 {
        CapabilityFold::KIND_ID
    }

    fn summarize(&self, ctx: &SummarizerContext<'_>) -> Vec<SummaryAnnouncement> {
        let Some(fold) = ctx.fold.capability_fold() else {
            return Vec::new();
        };
        let (idle, busy, reserved, faulty) = fold.with_state(|state| {
            let mut idle = 0u64;
            let mut busy = 0u64;
            let mut reserved = 0u64;
            let mut faulty = 0u64;
            for entry in state.entries.values() {
                match entry.payload.state {
                    NodeState::Idle => idle += 1,
                    NodeState::Busy => busy += 1,
                    NodeState::Reserved => reserved += 1,
                    NodeState::Faulty => faulty += 1,
                }
            }
            (idle, busy, reserved, faulty)
        });
        vec![SummaryAnnouncement {
            source_subnet: ctx.source_subnet,
            fold_kind: CapabilityFold::KIND_ID,
            generation: ctx.generation,
            // Sorted lexicographically so receivers see stable wire
            // bytes across runs.
            buckets: vec![
                ("busy".to_string(), busy),
                ("faulty".to_string(), faulty),
                ("idle".to_string(), idle),
                ("reserved".to_string(), reserved),
            ],
        }]
    }
}

/// Built-in summarizer for `ReservationFold`. Emits one
/// announcement carrying per-`ReservationState` counts across
/// every reservation the fold currently tracks.
pub struct ReservationFoldSummarizer;

impl Summarizer for ReservationFoldSummarizer {
    fn fold_kind(&self) -> u16 {
        ReservationFold::KIND_ID
    }

    fn summarize(&self, ctx: &SummarizerContext<'_>) -> Vec<SummaryAnnouncement> {
        use crate::adapter::net::behavior::fold::reservation::ReservationState;
        let Some(fold) = ctx.fold.reservation_fold() else {
            return Vec::new();
        };
        // Match each variant to a fixed `&'static str` label. The
        // previous shape used `format!("{:?}").to_lowercase()`
        // per entry, allocating once per reservation per tick;
        // this version allocates three short Strings per tick
        // total (one per bucket-name slot in the wire payload).
        let (free, reserved, active) = fold.with_state(|state| {
            let mut free = 0u64;
            let mut reserved = 0u64;
            let mut active = 0u64;
            for entry in state.entries.values() {
                match entry.payload.state {
                    ReservationState::Free => free += 1,
                    ReservationState::Reserved { .. } => reserved += 1,
                    ReservationState::Active { .. } => active += 1,
                }
            }
            (free, reserved, active)
        });
        vec![SummaryAnnouncement {
            source_subnet: ctx.source_subnet,
            fold_kind: ReservationFold::KIND_ID,
            generation: ctx.generation,
            // Lex-sorted on bucket name for stable wire bytes.
            buckets: vec![
                ("active".to_string(), active),
                ("free".to_string(), free),
                ("reserved".to_string(), reserved),
            ],
        }]
    }
}

/// Resolve a `fold_kind` to either the matching built-in
/// summarizer or the operator's custom-registered one. Returns
/// `None` when neither is available — the aggregator daemon
/// treats that as a configuration error at startup.
pub fn resolve_summarizer(
    fold_kind: u16,
    custom: &std::collections::HashMap<u16, Arc<dyn Summarizer>>,
) -> Option<Arc<dyn Summarizer>> {
    if let Some(custom) = custom.get(&fold_kind) {
        return Some(custom.clone());
    }
    if fold_kind == CapabilityFold::KIND_ID {
        return Some(Arc::new(CapabilityFoldSummarizer));
    }
    if fold_kind == ReservationFold::KIND_ID {
        return Some(Arc::new(ReservationFoldSummarizer));
    }
    None
}

/// `FoldHandle` impl backed by a concrete capability fold.
/// Lets the aggregator daemon hand a `&dyn FoldHandle` to a
/// summarizer with one allocation per tick.
pub struct CapabilityFoldHandle<'a>(pub &'a Fold<CapabilityFold>);

impl FoldHandle for CapabilityFoldHandle<'_> {
    fn capability_fold(&self) -> Option<&Fold<CapabilityFold>> {
        Some(self.0)
    }
}

/// `FoldHandle` impl backed by a concrete reservation fold.
pub struct ReservationFoldHandle<'a>(pub &'a Fold<ReservationFold>);

impl FoldHandle for ReservationFoldHandle<'_> {
    fn reservation_fold(&self) -> Option<&Fold<ReservationFold>> {
        Some(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::capability::CapabilityMembership;
    use crate::adapter::net::behavior::fold::wire::{EnvelopeMeta, SignedAnnouncement};
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn sign_cap(
        kp: &EntityKeypair,
        publisher: u64,
        class: u64,
        state: NodeState,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: Vec::new(),
                hardware: None,
                state,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    #[test]
    fn capability_summarizer_buckets_by_state_with_lex_sorted_keys() {
        let fold: Fold<CapabilityFold> = Fold::with_sweep_interval(Duration::ZERO);
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
        fold.apply(sign_cap(&kp, 0xB, 2, NodeState::Idle)).unwrap();
        fold.apply(sign_cap(&kp, 0xC, 3, NodeState::Busy)).unwrap();
        fold.apply(sign_cap(&kp, 0xD, 4, NodeState::Faulty))
            .unwrap();

        let handle = CapabilityFoldHandle(&fold);
        let ctx = SummarizerContext {
            source_subnet: SubnetId::new(&[3, 7]),
            generation: 42,
            fold: &handle,
        };
        let out = CapabilityFoldSummarizer.summarize(&ctx);
        assert_eq!(out.len(), 1);
        let summary = &out[0];
        assert_eq!(summary.source_subnet, SubnetId::new(&[3, 7]));
        assert_eq!(summary.fold_kind, CapabilityFold::KIND_ID);
        assert_eq!(summary.generation, 42);
        // Lex-sorted: busy, faulty, idle, reserved
        let bucket_names: Vec<&str> = summary.buckets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(bucket_names, vec!["busy", "faulty", "idle", "reserved"]);
        let bucket_counts: Vec<u64> = summary.buckets.iter().map(|(_, c)| *c).collect();
        assert_eq!(bucket_counts, vec![1, 1, 2, 0]);
    }

    #[test]
    fn resolve_summarizer_returns_builtin_for_known_kinds() {
        let custom: std::collections::HashMap<u16, Arc<dyn Summarizer>> =
            std::collections::HashMap::new();
        let s = resolve_summarizer(CapabilityFold::KIND_ID, &custom).expect("builtin capability");
        assert_eq!(s.fold_kind(), CapabilityFold::KIND_ID);
        let s = resolve_summarizer(ReservationFold::KIND_ID, &custom).expect("builtin reservation");
        assert_eq!(s.fold_kind(), ReservationFold::KIND_ID);
        // Unknown kind without a custom registration returns None.
        assert!(resolve_summarizer(0xDEAD, &custom).is_none());
    }

    #[test]
    fn resolve_summarizer_custom_overrides_builtin() {
        struct StubSummarizer;
        impl Summarizer for StubSummarizer {
            fn fold_kind(&self) -> u16 {
                CapabilityFold::KIND_ID
            }
            fn summarize(&self, _ctx: &SummarizerContext<'_>) -> Vec<SummaryAnnouncement> {
                vec![SummaryAnnouncement {
                    source_subnet: SubnetId::GLOBAL,
                    fold_kind: CapabilityFold::KIND_ID,
                    generation: 1,
                    buckets: vec![("custom".into(), 1)],
                }]
            }
        }
        let mut custom: std::collections::HashMap<u16, Arc<dyn Summarizer>> =
            std::collections::HashMap::new();
        custom.insert(CapabilityFold::KIND_ID, Arc::new(StubSummarizer));
        let s = resolve_summarizer(CapabilityFold::KIND_ID, &custom).expect("custom");
        // Sanity: the custom summarizer's `fold_kind` matches and
        // its summarize returns the stub payload (proves we got
        // the override, not the builtin).
        assert_eq!(s.fold_kind(), CapabilityFold::KIND_ID);
        let handle_fold: Fold<CapabilityFold> = Fold::with_sweep_interval(Duration::ZERO);
        let handle = CapabilityFoldHandle(&handle_fold);
        let ctx = SummarizerContext {
            source_subnet: SubnetId::GLOBAL,
            generation: 1,
            fold: &handle,
        };
        let out = s.summarize(&ctx);
        assert_eq!(out[0].buckets[0].0, "custom");
    }
}
