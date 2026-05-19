//! Per-stream bandwidth class for the v0.3 Phase D blob path.
//!
//! v0.3 Phase D's plan calls for fetch/store calls to take a
//! [`BandwidthClass`] parameter so the replication-budget
//! admission gate + per-channel send-queue priority + anti-
//! starvation hatch can distinguish "interactive 10 MiB chunk
//! fetch" from "1 TiB background backfill". Without per-stream
//! classification, a backfill saturates the per-channel rate and
//! starves interactive workloads — the failure mode the v0.3
//! plan calls out as the common case at TB scale.
//!
//! # Scope of this module (Phase D1)
//!
//! D1 ships the *declarative surface*:
//! - [`BandwidthClass`] enum with `Foreground`, `Background`,
//!   `Realtime` variants matching the plan §7.
//! - [`DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED`] capability tag.
//! - [`BandwidthClassSupportProbe`] hook for the cross-version
//!   downgrade decision (mirrors the A6/B2/C8 probe pattern).
//!
//! What's NOT in D1 (deferred to follow-up D-commits):
//! - D2: replication-budget admission gating that actually
//!   consults the class.
//! - D3: per-channel send-queue priority sort by class.
//! - D4: anti-starvation hatch (`Background` → `Foreground`
//!   promotion at 60 s starve).
//!
//! SDK consumers can start adopting the API today; the substrate
//! treats the parameter as a hint until the queue + admission
//! integration lands. This matches the v0.3 pattern of shipping
//! types ahead of full wiring (Phase A8 conformance tests pin
//! shape; subsequent commits add behavior).

/// Capability tag a node advertises when it accepts the v0.3
/// Phase D [`BandwidthClass`] hint on store/fetch calls.
/// Independent of the Tree/CDC/RS tags; a node can run any of
/// Phase A/B/C without Phase D's bandwidth-class surface.
///
/// Producers targeting a peer that doesn't advertise this tag
/// silently drop the bandwidth-class hint — the substrate
/// defaults all calls to [`BandwidthClass::Foreground`] when the
/// hint is absent, so the missing capability degrades gracefully
/// (no fetch/store fails; bandwidth shaping just doesn't apply
/// to the legacy peer).
pub const DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED: &str =
    "dataforts:blob-bandwidth-class-supported";

// `BandwidthClass` lives canonically in the redex layer (see
// `crate::adapter::net::redex::bandwidth`) so the replication
// runtime can consume it without a dataforts→replication
// dependency. Re-exported here so existing dataforts-blob
// consumers see no surface change.
pub use crate::adapter::net::redex::BandwidthClass;

/// Producer-side hook for the bandwidth-class downgrade decision.
///
/// Mirrors [`super::blob_tree::TreeSupportProbe`] /
/// [`super::cdc::CdcSupportProbe`] /
/// [`super::erasure::ErasureSupportProbe`] one-for-one — same
/// enum shape, same default-impl, same dynamic-closure variant
/// for runtime flag-driven decisions.
///
/// Behavior: callers pass a [`BandwidthClass`] alongside a probe;
/// on `probe.check() == false` the substrate substitutes
/// [`BandwidthClass::Foreground`] (the conservative choice — a
/// Background-class request that lands on a non-D-aware peer is
/// served as Foreground rather than failing). Foreground stays
/// Foreground; Realtime degrades to Foreground (preserves
/// liveness; gives up the rate-bypass that the legacy peer can't
/// honour anyway).
#[derive(Default)]
pub enum BandwidthClassSupportProbe {
    /// All targets support the class hint. Default for single-
    /// cluster all-Phase-D deployments.
    #[default]
    AlwaysSupported,
    /// No target supports the class hint. Forces every call to
    /// `Foreground`. Useful during cluster-wide rollouts.
    ForceForeground,
    /// Dynamic check — caller-supplied closure consults the
    /// capability-tag advertisement layer at decision time.
    /// Returns `true` iff the destination advertises
    /// [`DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED`].
    Dynamic(Box<dyn Fn() -> bool + Send + Sync>),
}

impl BandwidthClassSupportProbe {
    /// Evaluate the probe. Cheap for the static variants; invokes
    /// the closure for `Dynamic`.
    pub fn check(&self) -> bool {
        match self {
            BandwidthClassSupportProbe::AlwaysSupported => true,
            BandwidthClassSupportProbe::ForceForeground => false,
            BandwidthClassSupportProbe::Dynamic(f) => f(),
        }
    }
}

impl std::fmt::Debug for BandwidthClassSupportProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BandwidthClassSupportProbe::AlwaysSupported => {
                f.write_str("BandwidthClassSupportProbe::AlwaysSupported")
            }
            BandwidthClassSupportProbe::ForceForeground => {
                f.write_str("BandwidthClassSupportProbe::ForceForeground")
            }
            BandwidthClassSupportProbe::Dynamic(_) => {
                f.write_str("BandwidthClassSupportProbe::Dynamic(..)")
            }
        }
    }
}

/// Producer-side downgrade helper: if `probe.check()` returns
/// `false`, collapse any non-`Foreground` class to `Foreground`.
/// Pass-through otherwise.
///
/// Composes with the [`super::cdc::cdc_downgrade`] and
/// [`super::erasure::erasure_downgrade`] helpers — callers
/// consult Tree, CDC, RS, and bandwidth-class probes
/// independently before invoking `store_stream_tree`/`fetch_range`.
pub fn bandwidth_class_downgrade(
    class: BandwidthClass,
    probe: &BandwidthClassSupportProbe,
) -> BandwidthClass {
    if probe.check() {
        class
    } else {
        BandwidthClass::Foreground
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_class_is_foreground() {
        assert_eq!(BandwidthClass::default(), BandwidthClass::Foreground);
    }

    #[test]
    fn capability_tag_matches_plan() {
        assert_eq!(
            DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED,
            "dataforts:blob-bandwidth-class-supported"
        );
    }

    #[test]
    fn support_probe_static_variants() {
        assert!(BandwidthClassSupportProbe::AlwaysSupported.check());
        assert!(!BandwidthClassSupportProbe::ForceForeground.check());
        assert!(BandwidthClassSupportProbe::default().check());
    }

    #[test]
    fn support_probe_dynamic_consults_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let probe =
            BandwidthClassSupportProbe::Dynamic(Box::new(move || f.load(Ordering::Relaxed)));
        assert!(!probe.check());
        flag.store(true, Ordering::Relaxed);
        assert!(probe.check());
    }

    #[test]
    fn downgrade_collapses_to_foreground_on_reject() {
        let probe_yes = BandwidthClassSupportProbe::AlwaysSupported;
        let probe_no = BandwidthClassSupportProbe::ForceForeground;
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Background, &probe_yes),
            BandwidthClass::Background
        );
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Realtime, &probe_yes),
            BandwidthClass::Realtime
        );
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Background, &probe_no),
            BandwidthClass::Foreground
        );
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Realtime, &probe_no),
            BandwidthClass::Foreground
        );
        // Foreground is idempotent under downgrade.
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Foreground, &probe_yes),
            BandwidthClass::Foreground
        );
        assert_eq!(
            bandwidth_class_downgrade(BandwidthClass::Foreground, &probe_no),
            BandwidthClass::Foreground
        );
    }
}
